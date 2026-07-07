use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::TOKEN_URL;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SmartcodeDriveUpload {
    pub job_id: u64,
    pub file_id: String,
    pub folder_id: String,
    pub url: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
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
            delete_drive_file(server_id, &previous.file_id).await
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
    serde_json::from_str(&raw).map(Some).map_err(|e| e.to_string())
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
    tokio::fs::write(path, raw).await.map_err(|e| e.to_string())
}

fn state_path(server_id: u64, channel_id: u64, episode: u32) -> PathBuf {
    PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join(channel_id.to_string())
        .join("smartcode_drive")
        .join(format!("{:02}.json", episode))
}

async fn delete_drive_file(server_id: u64, file_id: &str) -> Result<(), String> {
    let creds = drive_credentials(server_id)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let access_token = access_token(&client, &creds).await?;
    let resp = client
        .delete(format!(
            "https://www.googleapis.com/drive/v3/files/{}",
            path_escape(file_id)
        ))
        .bearer_auth(access_token)
        .query(&[("supportsAllDrives", "true")])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
        Ok(())
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Drive delete failed for `{}`: {} {}", file_id, status, body))
    }
}

async fn access_token(client: &Client, creds: &DriveCredentials) -> Result<String, String> {
    let params = [
        ("client_id", creds.client_id.as_str()),
        ("client_secret", creds.client_secret.as_str()),
        ("refresh_token", creds.refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ];
    let resp = client
        .post(&creds.token_url)
        .form(&params)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("Drive token request failed: {} {}", status, body));
    }
    serde_json::from_str::<TokenResponse>(&body)
        .map(|token| token.access_token)
        .map_err(|e| format!("Drive token response decode failed: {}; body: {}", e, body))
}

struct DriveCredentials {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    token_url: String,
}

fn drive_credentials(server_id: u64) -> Result<DriveCredentials, String> {
    let meta_path = PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join("meta.pandora");
    let meta = std::fs::read_to_string(&meta_path)
        .map_err(|e| format!("failed to read `{}`: {}", meta_path.display(), e))?;
    let lines = meta.lines().collect::<Vec<_>>();
    if matches!(
        lines.get(9).copied().unwrap_or("true").trim(),
        "false" | "0" | "disabled" | "off"
    ) {
        return Err("server local Google Drive uploads are disabled".to_string());
    }
    let client_id = lines.get(4).copied().unwrap_or("").trim().to_string();
    let client_secret = lines.get(5).copied().unwrap_or("").trim().to_string();
    let refresh_token = lines.get(6).copied().unwrap_or("").trim().to_string();
    let token_url = get_pandora_env()
        .get(TOKEN_URL)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("global `{}` is not configured", TOKEN_URL))?;
    if client_id.is_empty() || client_secret.is_empty() || refresh_token.is_empty() {
        return Err("server Google Drive credentials are incomplete".to_string());
    }
    Ok(DriveCredentials {
        client_id,
        client_secret,
        refresh_token,
        token_url,
    })
}

fn path_escape(raw: &str) -> String {
    let mut out = String::new();
    for b in raw.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}
