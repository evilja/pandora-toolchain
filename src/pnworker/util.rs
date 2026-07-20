use crate::lib::protocol::core::{Protocol, TypeC};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[derive(Debug)]
pub enum CliParam {
    Literal(&'static str),
    JobId(&'static str),
    Path(&'static str),
    Flag(&'static str),
    NegVer(&'static str),
    RepeatedPath(&'static str),
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
    let mut cmd = Command::new(tool_path);
    for param in params {
        match param {
            CliParam::Literal(s) => {
                cmd.arg(s);
            }
            CliParam::Flag(s) => {
                cmd.arg(format!("--{}", s));
            }
            CliParam::JobId(prefix) => {
                cmd.arg(format!("{}{}", prefix, job_id));
            }
            CliParam::NegVer(v) => {
                cmd.arg(v);
            }
            CliParam::Path(key) => {
                if let Some(PathValue::Single(s)) = paths.get(key) {
                    cmd.arg(s);
                } else {
                    panic!("Missing or wrong type for path key: {}", key);
                }
            }
            CliParam::RepeatedPath(key) => {
                if let Some(PathValue::Multi(values)) = paths.get(key) {
                    for v in values {
                        cmd.arg("--candidate");
                        cmd.arg(v);
                    }
                }
            }
        }
    }
    cmd.stderr(Stdio::null());
    cmd.stdout(Stdio::piped());
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
    use super::migrate_intro_config_contents;

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
