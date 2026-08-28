use crate::lib::db::core::JobDb;
use crate::lumiere_broker::LumiereClient;
use crate::pnworker::core::{Job, JobType, Stage};
use crate::pnworker::messages::{
    MessagePayload, UPLOAD_BACKUP_PROG, UPLOAD_DONE, UPLOAD_PROG,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::sync::Mutex;

const STATE_DIRECTORY: &str = "DB/config/global/environment/drive_deletions";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SmartcodeReference {
    server_id: u64,
    channel_id: u64,
    episode: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct JobDriveUpload {
    job_id: u64,
    file_id: String,
    profile: String,
    delete_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    smartcode: Option<SmartcodeReference>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DriveCapability {
    file_id: String,
    profile: String,
    delete_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriveDeleteOutcome {
    Deleted { affected_jobs: Vec<u64> },
    NoCapability,
}

fn delete_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub fn drive_deletable_job_type(job_type: JobType) -> bool {
    matches!(
        job_type,
        JobType::Encode | JobType::Pancode | JobType::Keycode | JobType::Studio
    )
}

pub async fn persist_job_drive_upload(
    job: &Job,
    payload: &MessagePayload,
    stage: Option<Stage>,
) -> Result<(), String> {
    if stage != Some(Stage::Uploaded) || !drive_deletable_job_type(job.job_type) {
        return Ok(());
    }
    let Some(capability) = capability_from_payload(payload) else {
        return Ok(());
    };
    let smartcode = job
        .server_id
        .zip(job.smartcode_drive_name.as_ref())
        .map(|(server_id, name)| SmartcodeReference {
            server_id,
            channel_id: job.channel_id,
            episode: name.episode,
        });
    write_state(&JobDriveUpload {
        job_id: job.job_id,
        file_id: capability.file_id,
        profile: capability.profile,
        delete_token: capability.delete_token,
        smartcode,
    })
    .await
}

pub async fn delete_job_drive_upload(
    db: &JobDb,
    job_id: u64,
) -> Result<DriveDeleteOutcome, String> {
    let _guard = delete_lock().lock().await;
    let Some(requested) = read_state(job_id).await? else {
        return Ok(DriveDeleteOutcome::NoCapability);
    };
    delete_drive_file_unlocked(
        &requested.profile,
        &requested.file_id,
        &requested.delete_token,
    )
    .await?;

    let matching = matching_states(&requested.profile, &requested.file_id).await;
    let mut affected = BTreeMap::new();
    affected.insert(requested.job_id, requested);
    for state in matching {
        affected.insert(state.job_id, state);
    }

    let mut errors = Vec::new();
    for (affected_job, state) in &affected {
        if let Some(reference) = state.smartcode.as_ref()
            && let Err(error) = crate::pnworker::smartcode_drive::remove_smartcode_upload_if_job(
                reference.server_id,
                reference.channel_id,
                reference.episode,
                *affected_job,
            )
            .await
        {
            errors.push(format!("clear Smartcode state for job {affected_job}: {error}"));
        }
        if let Err(error) = redact_job_drive_link(db, *affected_job).await {
            errors.push(format!("redact job {affected_job}: {error}"));
        }
        if let Err(error) = tokio::fs::remove_file(state_path(*affected_job)).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            errors.push(format!("remove capability for job {affected_job}: {error}"));
        }
    }
    if !errors.is_empty() {
        eprintln!(
            "[drive-delete] Google Drive removed for job {job_id}, but local cleanup was incomplete: {}",
            errors.join("; ")
        );
    }
    Ok(DriveDeleteOutcome::Deleted {
        affected_jobs: affected.into_keys().collect(),
    })
}

// Smartcode replacement predates reaction deletion. It can delete a legacy file from its own
// state even when no per-job state exists; when one does exist, consume every copy so a later
// reaction cannot retry a capability whose file has already gone.
pub async fn delete_replaced_drive_upload(
    profile: &str,
    file_id: &str,
    delete_token: &str,
) -> Result<(), String> {
    let _guard = delete_lock().lock().await;
    delete_drive_file_unlocked(profile, file_id, delete_token).await?;
    for state in matching_states(profile, file_id).await {
        tokio::fs::remove_file(state_path(state.job_id)).await.ok();
    }
    Ok(())
}

async fn delete_drive_file_unlocked(
    profile: &str,
    file_id: &str,
    delete_token: &str,
) -> Result<(), String> {
    if profile.trim().is_empty() || delete_token.trim().is_empty() {
        return Err("legacy Drive file requires manual cleanup".to_string());
    }
    LumiereClient::from_env()
        .map_err(|error| error.to_string())?
        .delete_drive_file(
            profile.trim().to_string(),
            file_id.trim().to_string(),
            delete_token.trim().to_string(),
        )
        .await
        .map_err(|error| error.to_string())
}

async fn redact_job_drive_link(db: &JobDb, job_id: u64) -> Result<(), String> {
    let Some(row) = db.get_job(job_id).await.map_err(|error| error.to_string())? else {
        return Ok(());
    };
    if let Some(value) = row.uploaded_links.as_deref().and_then(redact_uploaded_links) {
        db.update_links(job_id, &value.to_string())
            .await
            .map_err(|error| error.to_string())?;
    }
    if let Some(value) = row.progress.as_deref().and_then(redact_upload_progress) {
        db.update_progress(job_id, &value.to_string())
            .await
            .map_err(|error| error.to_string())?;
    }
    if let Some(value) = row.acix_pending.as_deref().and_then(redact_acix_pending) {
        db.set_acix_pending(job_id, &value.to_string())
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn redact_uploaded_links(raw: &str) -> Option<serde_json::Value> {
    let mut value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let object = value.as_object_mut()?;
    for key in ["drive", "drive_file_id", "drive_folder_id", "drive_profile"] {
        object.remove(key);
    }
    Some(value)
}

fn redact_upload_progress(raw: &str) -> Option<serde_json::Value> {
    let mut value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let hosts = value.get_mut("hosts")?.as_array_mut()?;
    if let Some(drive) = hosts.first_mut() {
        *drive = serde_json::Value::String(String::new());
    }
    Some(value)
}

fn redact_acix_pending(raw: &str) -> Option<serde_json::Value> {
    let mut value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let object = value.as_object_mut()?;
    if object.contains_key("drive") {
        object.insert("drive".to_string(), serde_json::Value::String(String::new()));
        Some(value)
    } else {
        None
    }
}

fn capability_from_payload(payload: &MessagePayload) -> Option<DriveCapability> {
    let MessagePayload::Progress(id, args) = payload else {
        return None;
    };
    if *id != UPLOAD_PROG && *id != UPLOAD_DONE && *id != UPLOAD_BACKUP_PROG {
        return None;
    }
    Some(DriveCapability {
        file_id: args.get(5).map(|value| value.trim()).filter(|value| !value.is_empty())?.to_string(),
        profile: args.get(7).map(|value| value.trim()).filter(|value| !value.is_empty())?.to_string(),
        delete_token: args.get(8).map(|value| value.trim()).filter(|value| !value.is_empty())?.to_string(),
    })
}

async fn write_state(state: &JobDriveUpload) -> Result<(), String> {
    let path = state_path(state.job_id);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    tokio::fs::set_permissions(parent, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .await
        .map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(".{}.{}.tmp", state.job_id, std::process::id()));
    let raw = serde_json::to_vec(state).map_err(|error| error.to_string())?;
    tokio::fs::write(&temporary, raw)
        .await
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    tokio::fs::set_permissions(&temporary, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .await
        .map_err(|error| error.to_string())?;
    match tokio::fs::rename(&temporary, &path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|error| error.to_string())?;
            tokio::fs::rename(&temporary, &path)
                .await
                .map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

async fn read_state(job_id: u64) -> Result<Option<JobDriveUpload>, String> {
    let path = state_path(job_id);
    let raw = match tokio::fs::read(&path).await {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    serde_json::from_slice(&raw)
        .map(Some)
        .map_err(|error| error.to_string())
}

async fn matching_states(profile: &str, file_id: &str) -> Vec<JobDriveUpload> {
    let mut matches = Vec::new();
    let mut entries = match tokio::fs::read_dir(STATE_DIRECTORY).await {
        Ok(entries) => entries,
        Err(_) => return matches,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = tokio::fs::read(&path).await else {
            continue;
        };
        let Ok(state) = serde_json::from_slice::<JobDriveUpload>(&raw) else {
            continue;
        };
        if state.profile == profile && state.file_id == file_id {
            matches.push(state);
        }
    }
    matches
}

fn state_path(job_id: u64) -> PathBuf {
    PathBuf::from(STATE_DIRECTORY).join(format!("{job_id}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_capability_is_read_after_the_visible_host_slots() {
        let payload = MessagePayload::Progress(
            UPLOAD_DONE,
            vec![
                "drive".into(), "byse".into(), "lulu".into(), "voe".into(), String::new(),
                "file-id".into(), "folder-id".into(), "guild:1".into(), "delete-token".into(),
                "smartcode".into(),
            ],
        );
        assert_eq!(
            capability_from_payload(&payload),
            Some(DriveCapability {
                file_id: "file-id".into(),
                profile: "guild:1".into(),
                delete_token: "delete-token".into(),
            })
        );
    }

    #[test]
    fn non_release_encode_payloads_retain_the_same_private_capability() {
        let payload = MessagePayload::Progress(
            UPLOAD_BACKUP_PROG,
            vec![
                "drive".into(), String::new(), String::new(), String::new(), String::new(),
                "file-id".into(), "folder-id".into(), "global".into(), "delete-token".into(),
                "default".into(),
            ],
        );
        assert_eq!(capability_from_payload(&payload).unwrap().file_id, "file-id");
    }

    #[test]
    fn redaction_keeps_public_hosts_and_clears_every_drive_handle() {
        let links = redact_uploaded_links(
            r#"{"drive":"drive","byse":"byse","drive_file_id":"file","drive_folder_id":"folder","drive_profile":"guild:1","warnings":["warning"]}"#,
        )
        .unwrap();
        assert!(links.get("drive").is_none());
        assert!(links.get("drive_file_id").is_none());
        assert_eq!(links["byse"], "byse");
        assert_eq!(links["warnings"][0], "warning");

        let progress = redact_upload_progress(
            r#"{"type":"upload","percent":100,"hosts":["drive","byse","lulu","voe",""]}"#,
        )
        .unwrap();
        assert_eq!(progress["hosts"][0], "");
        assert_eq!(progress["hosts"][1], "byse");

        let acix = redact_acix_pending(
            r#"{"status":"pending","drive":"drive","multiple_status":"pending"}"#,
        )
        .unwrap();
        assert_eq!(acix["drive"], "");
        assert_eq!(acix["multiple_status"], "pending");
    }

    #[test]
    fn only_upload_producing_encode_jobs_are_deletable() {
        for job_type in [JobType::Encode, JobType::Pancode, JobType::Keycode, JobType::Studio] {
            assert!(drive_deletable_job_type(job_type));
        }
        assert!(!drive_deletable_job_type(JobType::Backup));
        assert!(!drive_deletable_job_type(JobType::Preview));
        assert!(!drive_deletable_job_type(JobType::Batch));
    }
}
