use crate::lib::protocol::core::{Protocol, TypeC};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub const OUTPUT_RESOLUTION_FILE: &str = "output_resolution.pandora";

// Where one job's work directory lives, absolute.
//
// Everything under `DB/` is addressed relatively, and `std::env::current_dir()` is how the absolute
// forms of those paths were being built — separately, at each call site, each with its own
// `unwrap_or(".")`. That is fine while the call always succeeds and a silent disagreement the
// moment it does not: a node writing its encode to one path and a link route looking for it at
// another produces "the node reported a returned output that is not on disk", which names neither
// the cause nor the directory. Resolving it once means every caller is wrong together or right
// together, and a process whose own working directory it cannot read says so at the first call
// rather than quietly relocating half of `DB/`.
pub fn job_work_dir(job_id: u64) -> PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!(
                "[Pandora] this process cannot read its own working directory ({error}); DB paths will be relative to wherever it was started"
            );
            PathBuf::new()
        }
    })
    .join("DB")
    .join("work")
    .join(job_id.to_string())
}

#[derive(Debug)]
pub enum CliParam {
    Literal(&'static str),
    JobId(&'static str),
    Path(&'static str),
    Flag(&'static str),
    NegVer(&'static str),
    RepeatedPath(&'static str),
    // A flag and its value that are only passed when the caller supplied one: `--hls` is given to
    // the encodes whose server publishes HLS instead of an MP4 and left off every other one.
    OptionalPair(&'static str, &'static str),
}

pub enum ToolResult {
    Success,
    Fail,
    Cancel,
}

pub fn job_cancelled(directory: &Path) -> bool {
    directory.join("CANCEL").try_exists().unwrap_or(false)
}

pub struct WorkerNamePool {
    names: Vec<String>,
    used: HashSet<String>,
}

impl WorkerNamePool {
    pub fn new(names: Vec<String>) -> Self {
        Self {
            names,
            used: HashSet::new(),
        }
    }

    pub fn set_names(&mut self, names: Vec<String>) {
        self.names = names;
    }

    pub fn acquire(&mut self) -> Option<String> {
        let available: Vec<&str> = self
            .names
            .iter()
            .map(|name| name.as_str())
            .filter(|name| !self.used.contains(*name))
            .collect();
        if available.is_empty() {
            return None;
        }
        let mut bytes = [0u8; 8];
        let idx = if getrandom::getrandom(&mut bytes).is_ok() {
            (u64::from_ne_bytes(bytes) as usize) % available.len()
        } else {
            0
        };
        let name = available[idx].to_string();
        self.used.insert(name.clone());
        Some(name)
    }

    pub fn release(&mut self, name: &str) {
        self.used.remove(name);
    }
}

pub enum PathValue {
    Single(String),
    Multi(Vec<String>),
}

impl From<String> for PathValue {
    fn from(s: String) -> Self {
        PathValue::Single(s)
    }
}

impl From<Vec<String>> for PathValue {
    fn from(v: Vec<String>) -> Self {
        PathValue::Multi(v)
    }
}

// Turns a spec plus its call site's value map into the argv. A `Path` the caller forgot is a
// wiring mistake between `tools.rs` and the worker that declares nothing at compile time, so it is
// reported by name instead of building a command line that is quietly missing a value.
pub fn tool_args(
    params: &[CliParam],
    paths: &HashMap<&str, PathValue>,
    job_id: u64,
) -> Result<Vec<String>, String> {
    let mut args = Vec::with_capacity(params.len());
    for param in params {
        match param {
            CliParam::Literal(s) => args.push(s.to_string()),
            CliParam::Flag(s) => args.push(format!("--{}", s)),
            CliParam::JobId(prefix) => args.push(format!("{}{}", prefix, job_id)),
            CliParam::NegVer(v) => args.push(v.to_string()),
            CliParam::Path(key) => match paths.get(key) {
                Some(PathValue::Single(s)) => args.push(s.clone()),
                _ => return Err(format!("Missing or wrong type for path key: {}", key)),
            },
            CliParam::OptionalPair(flag, key) => {
                if let Some(PathValue::Single(value)) = paths.get(key) {
                    args.push(flag.to_string());
                    args.push(value.clone());
                }
            }
            CliParam::RepeatedPath(key) => {
                if let Some(PathValue::Multi(values)) = paths.get(key) {
                    for v in values {
                        args.push("--candidate".to_string());
                        args.push(v.clone());
                    }
                }
            }
        }
    }
    Ok(args)
}

pub async fn run_tool<F>(
    tool_path: &str,
    params: &[CliParam],
    paths: &HashMap<&str, PathValue>,
    job_id: u64,
    proto: &mut Protocol,
    mut on_line: F,
) -> ToolResult
where
    F: FnMut(&TypeC) -> Option<ToolResult>,
{
    // Failing the job beats panicking the worker task: the layer stays up, the caller reports the
    // job failed now rather than the stall watchdog doing it twenty minutes later, and the reason
    // is on stderr instead of in a dropped future.
    let args = match tool_args(params, paths, job_id) {
        Ok(args) => args,
        Err(reason) => {
            eprintln!("[Pandora] {} for job {}: {}", tool_path, job_id, reason);
            return ToolResult::Fail;
        }
    };
    let mut cmd = Command::new(tool_path);
    cmd.args(&args);
    cmd.stderr(Stdio::null());
    cmd.stdout(Stdio::piped());
    // The shrine aborts a layer whose heartbeat expired, which drops this future wherever it is
    // parked. Without kill_on_drop the tool keeps running with nobody reading it, and every reboot
    // of a wedged worker leaves another encoder behind competing for the machine.
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().expect("Failed to spawn tool");
    let stdout = child.stdout.take().expect("No stdout");
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut negotiated = false;

    while let Ok(Some(line)) = lines.next_line().await {
        println!("{}", line);
        if !negotiated {
            if proto.negotiate(&line).is_ok() {
                negotiated = true;
            }
        } else if let Ok(data) = proto.extract_data(&line) {
            if let Some(result) = on_line(&data) {
                child.kill().await.ok();
                return result;
            }
        }
    }

    match child
        .wait()
        .await
        .expect("Failed to wait on child")
        .success()
    {
        true => ToolResult::Success,
        false => ToolResult::Fail,
    }
}

pub const INTROS_PATH: &str = "DB/config/global/environment/intros.toml";
pub const OUTROS_PATH: &str = "DB/config/global/environment/outros.toml";

// Which end of the episode a concat group is stitched onto. An intro and an outro are the same kind
// of thing — a folder of retained variants pnmpeg picks a stream-compatible one out of — and differ
// only in the config they are registered in and the side of the encode they land on. Everything
// that handles one handles the other by carrying this rather than by having a second copy.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConcatKind {
    Intro,
    Outro,
}

pub const CONCAT_KINDS: [ConcatKind; 2] = [ConcatKind::Intro, ConcatKind::Outro];

impl ConcatKind {
    pub fn config_path(self) -> &'static str {
        match self {
            ConcatKind::Intro => INTROS_PATH,
            ConcatKind::Outro => OUTROS_PATH,
        }
    }

    // The lowercase noun this kind is spelled with everywhere an operator sees it: the `/touchintro`
    // and `/touchouttro` command names, the `concat` and `outro` option help, and error text.
    pub fn label(self) -> &'static str {
        match self {
            ConcatKind::Intro => "intro",
            ConcatKind::Outro => "outro",
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Default)]
pub struct ConcatConfig {
    pub groups: HashMap<String, String>,
}

#[derive(Deserialize)]
struct LegacyIntrosConfig {
    groups: HashMap<String, IntroGroupValue>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum IntroGroupValue {
    Folder(String),
    Files(Vec<String>),
}

impl ConcatConfig {
    pub fn load() -> Self {
        ConcatConfig::load_kind(ConcatKind::Intro)
    }

    // Only intros ever had the legacy file-list form, so only they are migrated on read. `outros.toml`
    // was born as a folder map and a parse failure there is a broken file, not an old one.
    pub fn load_kind(kind: ConcatKind) -> Self {
        let contents = match std::fs::read_to_string(kind.config_path()) {
            Ok(contents) => contents,
            Err(_) => return ConcatConfig::default(),
        };
        if contents.trim().is_empty() {
            return ConcatConfig::default();
        }
        if let Ok(config) = toml::from_str::<ConcatConfig>(&contents) {
            return config;
        }
        if kind != ConcatKind::Intro {
            eprintln!("Warning: could not parse {}", kind.config_path());
            return ConcatConfig::default();
        }
        match migrate_intro_config_contents(&contents) {
            Ok((config, true)) => {
                if let Err(e) = write_concat_config(ConcatKind::Intro, &config) {
                    eprintln!("Warning: failed to save migrated intro config: {}", e);
                }
                config
            }
            Ok((config, false)) => config,
            Err(e) => {
                eprintln!("Warning: failed to migrate intro config: {}", e);
                ConcatConfig::default()
            }
        }
    }

    pub fn resolve(&self, group: &str) -> Option<String> {
        self.groups.get(group).filter(|folder| !folder.trim().is_empty()).cloned()
    }

    // A job snapshots the folder its concat group resolved to, not the group's name, because that
    // is all the encoder needs. Naming it again — to tell a linked node which intro or outro to
    // sync — means asking the config which group that folder belongs to, rather than re-reading the
    // server's settings, which may have changed since the job was created.
    pub fn group_for_folder(&self, folder: &str) -> Option<String> {
        let folder = folder.trim();
        if folder.is_empty() {
            return None;
        }
        let mut names = self
            .groups
            .iter()
            .filter(|(_, value)| value.trim() == folder)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        names.sort();
        names.into_iter().next()
    }
}

pub fn migrate_intro_config() -> Result<bool, String> {
    let contents = match std::fs::read_to_string(ConcatKind::Intro.config_path()) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    if contents.trim().is_empty() {
        return Ok(false);
    }
    let (config, migrated) = migrate_intro_config_contents(&contents)?;
    if migrated {
        write_concat_config(ConcatKind::Intro, &config)?;
    }
    Ok(migrated)
}

fn migrate_intro_config_contents(contents: &str) -> Result<(ConcatConfig, bool), String> {
    let raw: LegacyIntrosConfig = toml::from_str(contents).map_err(|e| e.to_string())?;
    let mut config = ConcatConfig::default();
    let mut migrated = false;
    for (name, value) in raw.groups {
        let folder = match value {
            IntroGroupValue::Folder(folder) => folder,
            IntroGroupValue::Files(files) => {
                migrated = true;
                migrate_intro_group(&name, &files)?
            }
        };
        config.groups.insert(name, folder);
    }
    Ok((config, migrated))
}

fn migrate_intro_group(name: &str, files: &[String]) -> Result<String, String> {
    let parent = files
        .first()
        .and_then(|file| Path::new(file).parent())
        .filter(|first| files.iter().all(|file| Path::new(file).parent() == Some(*first)))
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("DB").join("concat").join("migrated"));
    let folder = parent.join(intro_folder_name(name));
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    for (index, file) in files.iter().enumerate() {
        let source = Path::new(file);
        if !source.is_file() {
            eprintln!("Warning: intro migration skipped missing file `{}`", source.display());
            continue;
        }
        let file_name = source.file_name()
            .and_then(|value| value.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("intro_{}.mp4", index));
        let mut destination = folder.join(&file_name);
        if destination.exists() && source.canonicalize().ok() != destination.canonicalize().ok() {
            let stem = Path::new(&file_name).file_stem().and_then(|v| v.to_str()).unwrap_or("intro");
            let ext = Path::new(&file_name).extension().and_then(|v| v.to_str()).unwrap_or("mp4");
            destination = folder.join(format!("{}_{}.{}", stem, index, ext));
        }
        if source.canonicalize().ok() == destination.canonicalize().ok() {
            continue;
        }
        if let Err(link_error) = std::fs::hard_link(source, &destination) {
            std::fs::copy(source, &destination).map_err(|copy_error| {
                format!(
                    "failed to migrate `{}` (hard link: {}; copy: {})",
                    source.display(), link_error, copy_error
                )
            })?;
        }
    }
    Ok(folder.display().to_string())
}

fn intro_folder_name(name: &str) -> String {
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return name.to_string();
    }
    format!("intro-{:x}", md5::compute(name.as_bytes()))
}

pub fn write_concat_config(kind: ConcatKind, config: &ConcatConfig) -> Result<(), String> {
    let path = kind.config_path();
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
    let temporary = format!("{}.tmp", path);
    std::fs::write(&temporary, body).map_err(|e| e.to_string())?;
    match std::fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(first) if Path::new(path).exists() => {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
            std::fs::rename(&temporary, path)
                .map_err(|e| format!("{}; replacement failed: {}", first, e))
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod intro_tests {
    use super::{ConcatConfig, ConcatKind, INTROS_PATH, OUTROS_PATH, migrate_intro_config_contents};

    // A job snapshots the folder its group resolved to, so naming the group again is a reverse
    // lookup. Two groups can legitimately point at one folder; the answer has to be stable rather
    // than whichever the map happened to yield first.
    #[test]
    fn a_folder_resolves_back_to_its_intro_group() {
        let mut groups = std::collections::HashMap::new();
        groups.insert("summer".to_string(), "DB/intros/summer".to_string());
        groups.insert("winter".to_string(), "DB/intros/winter".to_string());
        groups.insert("alias".to_string(), "DB/intros/summer".to_string());
        let config = ConcatConfig { groups };

        assert_eq!(config.group_for_folder("DB/intros/winter"), Some("winter".to_string()));
        assert_eq!(config.group_for_folder("DB/intros/summer"), Some("alias".to_string()));
        assert_eq!(
            config.group_for_folder("DB/intros/summer"),
            config.group_for_folder("DB/intros/summer")
        );
        assert_eq!(config.group_for_folder("DB/intros/nothing"), None);
        assert_eq!(config.group_for_folder("   "), None);
    }

    // The two kinds are separate registries in separate files. Sharing one would make an intro
    // group named `summer` and an outro group named `summer` the same entry, which is exactly the
    // collision the folder roots are kept apart to avoid.
    #[test]
    fn each_concat_kind_reads_its_own_file() {
        assert_eq!(ConcatKind::Intro.config_path(), INTROS_PATH);
        assert_eq!(ConcatKind::Outro.config_path(), OUTROS_PATH);
        assert_ne!(ConcatKind::Intro.config_path(), ConcatKind::Outro.config_path());
        assert_eq!(ConcatKind::Intro.label(), "intro");
        assert_eq!(ConcatKind::Outro.label(), "outro");
    }

    #[test]
    fn folder_intro_config_needs_no_migration() {
        let (config, migrated) = migrate_intro_config_contents(
            "[groups]\nopening = \"DB/concat/1/opening\"\n",
        ).unwrap();
        assert!(!migrated);
        assert_eq!(config.groups.get("opening").map(String::as_str), Some("DB/concat/1/opening"));
    }

    #[test]
    fn legacy_files_are_retained_in_a_group_folder() {
        let root = std::env::temp_dir().join(format!(
            "pandora-intro-migration-{}-{:x}",
            std::process::id(),
            md5::compute(format!("{:?}", std::time::SystemTime::now()).as_bytes())
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("opening_24.mp4");
        std::fs::write(&source, b"intro").unwrap();
        let escaped = source.display().to_string().replace('\\', "\\\\");
        let contents = format!("[groups]\nopening = [\"{}\"]\n", escaped);
        let (config, migrated) = migrate_intro_config_contents(&contents).unwrap();
        let folder = std::path::PathBuf::from(config.groups.get("opening").unwrap());
        assert!(migrated);
        assert_eq!(std::fs::read(folder.join("opening_24.mp4")).unwrap(), b"intro");
        assert_eq!(std::fs::read(&source).unwrap(), b"intro");
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[inline]
pub fn string_byte_to_mb(s: &str) -> u16 {
    (s.parse::<u64>().unwrap_or(1) / 1024 / 1024) as u16
}
