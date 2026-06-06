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
        let scheme_end = trimmed.find("://").ok_or_else(|| {
            format!("forgejo line `{}` must start with `http://` or `https://`", trimmed)
        })?;
        let after_scheme = &trimmed[scheme_end + 3..];
        let slash_in_rest = after_scheme.find('/').ok_or_else(|| {
            format!(
                "forgejo line `{}` must include the org as a path segment, e.g. `https://git.example.com/MyOrg`",
                trimmed
            )
        })?;
        let host = trimmed[..scheme_end + 3 + slash_in_rest].to_string();
        let org = after_scheme[slash_in_rest + 1..].to_string();
        if org.is_empty() {
            return Err(format!("forgejo line `{}` has empty org", trimmed));
        }
        if org.contains('/') {
            return Err(format!(
                "forgejo line `{}` org must be a single path segment, got `{}`",
                trimmed, org
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
        let status = resp.status();
        if status.is_success() {
            return Ok(format!("{}/{}/{}", self.host, self.org, name));
        }
        let text = resp.text().await.unwrap_or_default();
        if status.as_u16() == 409 {
            return Ok(format!("{}/{}/{}", self.host, self.org, name));
        }
        Err(format!("create_repo failed: {} {}", status, text))
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
        let url = contents_url(&self.host, owner_repo, path)?;
        let body = serde_json::json!({
            "content": content_b64,
            "message": message,
        });
        let resp = self.client.post(url.clone())
            .bearer_auth(&self.token)
            .json(&body)
            .send().await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let s = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("create_file failed ({}): {} {} (POST {})", path, s, text, url));
        }
        Ok(())
    }

    pub async fn get_file_sha(&self, owner_repo: &str, path: &str) -> Result<Option<String>, String> {
        let url = contents_url(&self.host, owner_repo, path)?;
        let resp = self.client.get(url.clone())
            .bearer_auth(&self.token)
            .send().await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("get_file_sha failed ({}): {} {} (GET {})", path, status, text, url));
        }
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        let sha = body.get("sha").and_then(|v| v.as_str()).unwrap_or("").to_string();
        Ok(Some(sha))
    }

    pub async fn update_file(&self, owner_repo: &str, path: &str, content_b64: &str, sha: &str, message: &str) -> Result<(), String> {
        let url = contents_url(&self.host, owner_repo, path)?;
        let body = serde_json::json!({
            "content": content_b64,
            "message": message,
            "sha": sha,
        });
        let resp = self.client.put(url.clone())
            .bearer_auth(&self.token)
            .json(&body)
            .send().await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let s = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("update_file failed ({}): {} {} (PUT {})", path, s, text, url));
        }
        Ok(())
    }

    pub async fn upsert_file(&self, owner_repo: &str, path: &str, content_b64: &str, message: &str) -> Result<(), String> {
        match self.get_file_sha(owner_repo, path).await? {
            Some(sha) => self.update_file(owner_repo, path, content_b64, &sha, message).await,
            None => self.create_file(owner_repo, path, content_b64, message).await,
        }
    }

    pub async fn move_file(&self, owner_repo: &str, from_path: &str, to_path: &str, message: &str) -> Result<(), String> {
        let url = contents_url(&self.host, owner_repo, to_path)?;
        let body = serde_json::json!({
            "from_path": from_path,
            "message": message,
        });
        let resp = self.client.post(url.clone())
            .bearer_auth(&self.token)
            .json(&body)
            .send().await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let s = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("move_file failed ({} -> {}): {} {} (POST {})", from_path, to_path, s, text, url));
        }
        Ok(())
    }

    pub async fn delete_repo(&self, owner_repo: &str) -> Result<(), String> {
        let url = format!("{}/api/v1/repos/{}", self.host, owner_repo);
        let resp = self.client.delete(&url)
            .bearer_auth(&self.token)
            .send().await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let s = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("delete_repo failed ({}): {} {} (DELETE {})", owner_repo, s, text, url));
        }
        Ok(())
    }
}

fn contents_url(host: &str, owner_repo: &str, path: &str) -> Result<reqwest::Url, String> {
    let base = format!("{}/api/v1/repos/{}/contents/", host, owner_repo);
    reqwest::Url::parse(&base)
        .and_then(|u| u.join(path))
        .map_err(|e| format!("invalid contents URL ({}): {}", path, e))
}

pub fn base64_encode(input: &str) -> String {
    base64_encode_bytes(input.as_bytes())
}

pub fn base64_encode_bytes(input: &[u8]) -> String {
    const ALPH: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(((input.len() + 2) / 3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let b0 = input[i];
        let b1 = input[i + 1];
        let b2 = input[i + 2];
        out.push(ALPH[(b0 >> 2) as usize] as char);
        out.push(ALPH[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(ALPH[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char);
        out.push(ALPH[(b2 & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let b0 = input[i];
        out.push(ALPH[(b0 >> 2) as usize] as char);
        out.push(ALPH[((b0 & 0x03) << 4) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let b0 = input[i];
        let b1 = input[i + 1];
        out.push(ALPH[(b0 >> 2) as usize] as char);
        out.push(ALPH[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(ALPH[((b1 & 0x0F) << 2) as usize] as char);
        out.push('=');
    }
    out
}
