use crate::pnworker::drive_cleanup::delete_replaced_drive_upload;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SmartcodeDriveUpload {
    pub job_id: u64,
    pub file_id: String,
    pub folder_id: String,
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub delete_token: String,
    pub url: String,
}

pub async fn replace_smartcode_upload(
    server_id: u64,
    channel_id: u64,
    episode: u32,
    upload: SmartcodeDriveUpload,
) -> Result<(), String> {
    let previous = read_smartcode_upload(server_id, channel_id, episode).await?;
    let delete_result = if let Some(previous) = previous {
        if previous.file_id != upload.file_id {
            delete_replaced_drive_upload(&previous.profile, &previous.file_id, &previous.delete_token).await
        } else {
            Ok(())
        }
    } else {
        Ok(())
    };
    write_smartcode_upload(server_id, channel_id, episode, &upload).await?;
    delete_result
}

async fn read_smartcode_upload(
    server_id: u64,
    channel_id: u64,
    episode: u32,
) -> Result<Option<SmartcodeDriveUpload>, String> {
    let path = state_path(server_id, channel_id, episode);
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| e.to_string())
}

async fn write_smartcode_upload(
    server_id: u64,
    channel_id: u64,
    episode: u32,
    upload: &SmartcodeDriveUpload,
) -> Result<(), String> {
    let path = state_path(server_id, channel_id, episode);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(upload).map_err(|e| e.to_string())?;
    tokio::fs::write(&path, raw)
        .await
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub async fn remove_smartcode_upload_if_job(
    server_id: u64,
    channel_id: u64,
    episode: u32,
    job_id: u64,
) -> Result<(), String> {
    let path = state_path(server_id, channel_id, episode);
    let Some(upload) = read_smartcode_upload(server_id, channel_id, episode).await? else {
        return Ok(());
    };
    if upload.job_id != job_id {
        return Ok(());
    }
    tokio::fs::remove_file(path)
        .await
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error)
            }
        })
        .map_err(|error| error.to_string())
}

fn state_path(server_id: u64, channel_id: u64, episode: u32) -> PathBuf {
    PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join(channel_id.to_string())
        .join("smartcode_drive")
        .join(format!("{:02}.json", episode))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_state_defaults_broker_capabilities_to_empty() {
        let upload: SmartcodeDriveUpload = serde_json::from_str(
            r#"{"job_id":1,"file_id":"file","folder_id":"folder","url":"url"}"#,
        )
        .unwrap();
        assert!(upload.profile.is_empty());
        assert!(upload.delete_token.is_empty());
    }
}
