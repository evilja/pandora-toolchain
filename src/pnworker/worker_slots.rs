use serde::{Deserialize, Serialize};
use std::path::Path;

pub const WORKER_SLOTS_PATH: &str = "DB/config/global/environment/workers.toml";
pub const MAX_WORKER_SLOTS: usize = 16;

const DEFAULT_DOWNLOAD: &[&str] = &["kawari", "fuan", "odo", "shitai"];
const DEFAULT_UPLOAD: &[&str] = &["tsuki", "sora", "tenki", "suisei"];
const DEFAULT_PROBE: &[&str] = &["hoshi", "kumo"];

fn default_probe_slots() -> Vec<String> {
    DEFAULT_PROBE.iter().map(|s| s.to_string()).collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WorkerSlotKind {
    Download,
    Probe,
    Upload,
}

impl WorkerSlotKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "download" | "dwl" => Some(Self::Download),
            "probe" | "prb" => Some(Self::Probe),
            "upload" | "upl" => Some(Self::Upload),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Probe => "probe",
            Self::Upload => "upload",
        }
    }

    pub fn worker_prefix(self) -> &'static str {
        match self {
            Self::Download => "dwl",
            Self::Probe => "prb",
            Self::Upload => "upl",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkerSlotsConfig {
    pub download: Vec<String>,
    #[serde(default = "default_probe_slots")]
    pub probe: Vec<String>,
    pub upload: Vec<String>,
}

impl Default for WorkerSlotsConfig {
    fn default() -> Self {
        Self {
            download: DEFAULT_DOWNLOAD.iter().map(|s| s.to_string()).collect(),
            probe: default_probe_slots(),
            upload: DEFAULT_UPLOAD.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl WorkerSlotsConfig {
    pub fn slots(&self, kind: WorkerSlotKind) -> &[String] {
        match kind {
            WorkerSlotKind::Download => &self.download,
            WorkerSlotKind::Probe => &self.probe,
            WorkerSlotKind::Upload => &self.upload,
        }
    }

    fn slots_mut(&mut self, kind: WorkerSlotKind) -> &mut Vec<String> {
        match kind {
            WorkerSlotKind::Download => &mut self.download,
            WorkerSlotKind::Probe => &mut self.probe,
            WorkerSlotKind::Upload => &mut self.upload,
        }
    }
}

pub async fn load_worker_slots() -> WorkerSlotsConfig {
    let raw = tokio::fs::read_to_string(WORKER_SLOTS_PATH).await.ok();
    let Some(raw) = raw else {
        return WorkerSlotsConfig::default();
    };
    let mut cfg = toml::from_str::<WorkerSlotsConfig>(&raw).unwrap_or_default();
    normalize_slots(&mut cfg.download);
    normalize_slots(&mut cfg.probe);
    normalize_slots(&mut cfg.upload);
    if cfg.download.is_empty() {
        cfg.download = WorkerSlotsConfig::default().download;
    }
    if cfg.upload.is_empty() {
        cfg.upload = WorkerSlotsConfig::default().upload;
    }
    if cfg.probe.is_empty() {
        cfg.probe = WorkerSlotsConfig::default().probe;
    }
    cfg
}

pub async fn download_worker_slots() -> Vec<String> {
    load_worker_slots().await.download
}

pub async fn upload_worker_slots() -> Vec<String> {
    load_worker_slots().await.upload
}

pub async fn probe_worker_slots() -> Vec<String> {
    load_worker_slots().await.probe
}

pub async fn add_worker_slot(kind: WorkerSlotKind, name: &str) -> Result<usize, String> {
    let name = normalize_name(name)?;
    let mut cfg = load_worker_slots().await;
    let slots = cfg.slots_mut(kind);
    if slots.iter().any(|slot| slot == &name) {
        return Err(format!("{} worker `{}` already exists", kind.label(), name));
    }
    if slots.len() >= MAX_WORKER_SLOTS {
        return Err(format!(
            "{} workers are limited to {} slots",
            kind.label(),
            MAX_WORKER_SLOTS
        ));
    }
    slots.push(name);
    let len = slots.len();
    save_worker_slots(&cfg).await?;
    Ok(len)
}

pub async fn remove_worker_slot(kind: WorkerSlotKind, name: &str) -> Result<(), String> {
    let name = normalize_name(name)?;
    let mut cfg = load_worker_slots().await;
    let slots = cfg.slots_mut(kind);
    if slots.len() <= 1 {
        return Err(format!(
            "{} workers must keep at least one slot",
            kind.label()
        ));
    }
    let Some(pos) = slots.iter().position(|slot| slot == &name) else {
        return Err(format!("{} worker `{}` does not exist", kind.label(), name));
    };
    slots.remove(pos);
    save_worker_slots(&cfg).await
}

async fn save_worker_slots(cfg: &WorkerSlotsConfig) -> Result<(), String> {
    if let Some(parent) = Path::new(WORKER_SLOTS_PATH).parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let body = toml::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    tokio::fs::write(WORKER_SLOTS_PATH, body)
        .await
        .map_err(|e| e.to_string())
}

fn normalize_slots(slots: &mut Vec<String>) {
    let mut out = Vec::new();
    for slot in slots.iter() {
        if let Ok(name) = normalize_name(slot) {
            if !out.iter().any(|existing| existing == &name) {
                out.push(name);
            }
        }
    }
    *slots = out;
}

pub fn normalize_name(name: &str) -> Result<String, String> {
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() {
        return Err("worker name cannot be empty".to_string());
    }
    if name.len() > 24 {
        return Err("worker name must be 24 bytes or fewer".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(
            "worker name may only contain lowercase letters, numbers, `_`, and `-`".to_string(),
        );
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_probe_worker_kind_aliases() {
        assert_eq!(WorkerSlotKind::parse("probe"), Some(WorkerSlotKind::Probe));
        assert_eq!(WorkerSlotKind::parse("PRB"), Some(WorkerSlotKind::Probe));
        assert_eq!(WorkerSlotKind::Probe.label(), "probe");
        assert_eq!(WorkerSlotKind::Probe.worker_prefix(), "prb");
    }

    #[test]
    fn legacy_config_gets_default_probe_slots() {
        let cfg: WorkerSlotsConfig = toml::from_str(
            r#"
download = ["one"]
upload = ["two"]
"#,
        )
        .unwrap();

        assert_eq!(cfg.download, vec!["one"]);
        assert_eq!(cfg.probe, vec!["hoshi", "kumo"]);
        assert_eq!(cfg.upload, vec!["two"]);
        assert_eq!(cfg.slots(WorkerSlotKind::Probe), cfg.probe.as_slice());
    }
}
