use crate::lib::protocol::core::{Protocol, TypeC};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub const OUTPUT_RESOLUTION_FILE: &str = "output_resolution.pandora";

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

#[derive(Deserialize, Serialize, Debug, Default)]
pub struct IntrosConfig {
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

impl IntrosConfig {
    pub fn load() -> Self {
        let contents = match std::fs::read_to_string(INTROS_PATH) {
            Ok(contents) => contents,
            Err(_) => return IntrosConfig::default(),
        };
        if contents.trim().is_empty() {
            return IntrosConfig::default();
        }
        if let Ok(config) = toml::from_str::<IntrosConfig>(&contents) {
            return config;
        }
        match migrate_intro_config_contents(&contents) {
            Ok((config, true)) => {
                if let Err(e) = write_intro_config(&config) {
                    eprintln!("Warning: failed to save migrated intro config: {}", e);
                }
                config
            }
            Ok((config, false)) => config,
            Err(e) => {
                eprintln!("Warning: failed to migrate intro config: {}", e);
                IntrosConfig::default()
            }
        }
    }

    pub fn resolve(&self, group: &str) -> Option<String> {
        self.groups.get(group).filter(|folder| !folder.trim().is_empty()).cloned()
    }

    // A job snapshots the folder its intro group resolved to, not the group's name, because that is
    // all the encoder needs. Naming it again — to tell a linked node which intro to sync — means
    // asking the config which group that folder belongs to, rather than re-reading the server's
    // settings, which may have changed since the job was created.
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
    let contents = match std::fs::read_to_string(INTROS_PATH) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    if contents.trim().is_empty() {
        return Ok(false);
    }
    let (config, migrated) = migrate_intro_config_contents(&contents)?;
    if migrated {
        write_intro_config(&config)?;
    }
    Ok(migrated)
}

fn migrate_intro_config_contents(contents: &str) -> Result<(IntrosConfig, bool), String> {
    let raw: LegacyIntrosConfig = toml::from_str(contents).map_err(|e| e.to_string())?;
    let mut config = IntrosConfig::default();
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

fn write_intro_config(config: &IntrosConfig) -> Result<(), String> {
    if let Some(parent) = Path::new(INTROS_PATH).parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
    let temporary = format!("{}.tmp", INTROS_PATH);
    std::fs::write(&temporary, body).map_err(|e| e.to_string())?;
    match std::fs::rename(&temporary, INTROS_PATH) {
        Ok(()) => Ok(()),
        Err(first) if Path::new(INTROS_PATH).exists() => {
            std::fs::remove_file(INTROS_PATH).map_err(|e| e.to_string())?;
            std::fs::rename(&temporary, INTROS_PATH)
                .map_err(|e| format!("{}; replacement failed: {}", first, e))
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod intro_tests {
    use super::{IntrosConfig, migrate_intro_config_contents};

    // A job snapshots the folder its group resolved to, so naming the group again is a reverse
    // lookup. Two groups can legitimately point at one folder; the answer has to be stable rather
    // than whichever the map happened to yield first.
    #[test]
    fn a_folder_resolves_back_to_its_intro_group() {
        let mut groups = std::collections::HashMap::new();
        groups.insert("summer".to_string(), "DB/intros/summer".to_string());
        groups.insert("winter".to_string(), "DB/intros/winter".to_string());
        groups.insert("alias".to_string(), "DB/intros/summer".to_string());
        let config = IntrosConfig { groups };

        assert_eq!(config.group_for_folder("DB/intros/winter"), Some("winter".to_string()));
        assert_eq!(config.group_for_folder("DB/intros/summer"), Some("alias".to_string()));
        assert_eq!(
            config.group_for_folder("DB/intros/summer"),
            config.group_for_folder("DB/intros/summer")
        );
        assert_eq!(config.group_for_folder("DB/intros/nothing"), None);
        assert_eq!(config.group_for_folder("   "), None);
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
