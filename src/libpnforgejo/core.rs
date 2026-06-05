use crate::libpnenv::{
    core::get_env,
    standard::FORGEJO_API_KEY,
};
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

pub struct Forgejo {
    pub host: String,
    pub org: String,
    pub token: String,
    pub client: Client,
}

impl Forgejo {
    pub fn from_env(forgejo_line: String) -> Result<Self, String> {
        let env = get_env("env.pandora");
        if env.len() <= FORGEJO_API_KEY || env[FORGEJO_API_KEY].is_empty() {
            return Err("FORGEJO_API_KEY is not set in env.pandora".to_string());
        }
        let token = env[FORGEJO_API_KEY].clone();
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())?;

        let trimmed = forgejo_line.trim_end_matches('/');
        let last_slash = trimmed.rfind('/').ok_or_else(|| {
            format!("forgejo line `{}` is not a URL with a host", trimmed)
        })?;
        let host = trimmed[..last_slash].to_string();
        let org = trimmed[last_slash + 1..].to_string();
        if host.is_empty() || org.is_empty() {
            return Err(format!(
                "forgejo line `{}` must be a full URL including the org, e.g. `https://git.example.com/MyOrg`",
                trimmed
            ));
        }
        Ok(Forgejo { host, org, token, client })
    }

    pub async fn create_repo(&self, name: &str) -> Result<String, String> {
        let url = format!("{}/api/v1/orgs/{}/repos", self.host, self.org);
        let body = serde_json::json!({
            "name": name,
            "auto_init": true,
            "private": false,
        });
        let resp = self.client.post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send().await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let s = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("create_repo failed: {} {}", s, text));
        }
        Ok(format!("{}/{}/{}", self.host, self.org, name))
    }

    pub async fn list_contents(&self, owner_repo: &str, path: &str) -> Result<Vec<String>, String> {
        let url = if path.is_empty() {
            format!("{}/api/v1/repos/{}/contents", self.host, owner_repo)
        } else {
            format!("{}/api/v1/repos/{}/contents/{}", self.host, owner_repo, path)
        };
        let resp = self.client.get(&url)
            .bearer_auth(&self.token)
            .send().await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let s = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("list_contents failed: {} {}", s, text));
        }
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        let arr = match json.as_array() {
            Some(a) => a,
            None => return Ok(Vec::new()),
        };
        let mut names: Vec<String> = Vec::new();
        for entry in arr {
            let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let ty = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if ty == "dir" || ty == "file" {
                names.push(name);
            }
        }
        Ok(names)
    }

    pub async fn create_file(&self, owner_repo: &str, path: &str, content_b64: &str, message: &str) -> Result<(), String> {
        let url = format!("{}/api/v1/repos/{}/contents/{}", self.host, owner_repo, path);
        let body = serde_json::json!({
            "content": content_b64,
            "message": message,
        });
        let resp = self.client.post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send().await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let s = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("create_file failed ({}): {} {}", path, s, text));
        }
        Ok(())
    }
}

pub fn base64_encode(input: &str) -> String {
    const ALPH: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(((bytes.len() + 2) / 3) * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let b0 = bytes[i];
        let b1 = bytes[i + 1];
        let b2 = bytes[i + 2];
        out.push(ALPH[(b0 >> 2) as usize] as char);
        out.push(ALPH[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(ALPH[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char);
        out.push(ALPH[(b2 & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let b0 = bytes[i];
        out.push(ALPH[(b0 >> 2) as usize] as char);
        out.push(ALPH[((b0 & 0x03) << 4) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let b0 = bytes[i];
        let b1 = bytes[i + 1];
        out.push(ALPH[(b0 >> 2) as usize] as char);
        out.push(ALPH[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(ALPH[((b1 & 0x0F) << 2) as usize] as char);
        out.push('=');
    }
    out
}
