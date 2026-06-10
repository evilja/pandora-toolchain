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
    pub fn new(forgejo_line: String, api_key: String) -> Result<Self, String> {
        if api_key.is_empty() {
            return Err("forgejo API key is empty. Run /configure with the `api_key` option.".to_string());
        }
        let token = api_key;
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
        self.commit_file(owner_repo, path, content_b64, message).await
    }

    async fn commit_file(&self, owner_repo: &str, path: &str, content_b64: &str, message: &str) -> Result<(), String> {
        let branch = self.default_branch(owner_repo).await?;
        let head_ref = format!("heads/{}", branch);
        let head_sha = self.get_ref_sha(owner_repo, &head_ref).await?;
        let base_tree = self.get_commit_tree(owner_repo, &head_sha).await?;
        let blob_sha = self.create_blob(owner_repo, content_b64).await?;
        let tree_sha = self.create_tree(owner_repo, &base_tree, path, &blob_sha).await?;
        let commit_sha = self.create_commit(owner_repo, message, &tree_sha, &head_sha).await?;
        self.update_ref(owner_repo, &head_ref, &commit_sha).await
    }

    async fn default_branch(&self, owner_repo: &str) -> Result<String, String> {
        let url = format!("{}/api/v1/repos/{}", self.host, owner_repo);
        let resp = self.client.get(&url)
            .bearer_auth(&self.token)
            .send().await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("default_branch failed: {} {} (GET {})", status, text, url));
        }
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        let branch = body.get("default_branch").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if branch.is_empty() {
            return Err(format!("default_branch failed: no default_branch for {}", owner_repo));
        }
        Ok(branch)
    }

    async fn get_ref_sha(&self, owner_repo: &str, git_ref: &str) -> Result<String, String> {
        let url = git_url(&self.host, owner_repo, &format!("refs/{}", git_ref))?;
        let resp = self.client.get(url.clone())
            .bearer_auth(&self.token)
            .send().await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("get_ref_sha failed ({}): {} {} (GET {})", git_ref, status, text, url));
        }
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        let sha = ref_sha_from_value(&body);
        if sha.is_empty() {
            return Err(format!("get_ref_sha failed ({}): no object sha in {}", git_ref, body));
        }
        Ok(sha)
    }

    async fn get_commit_tree(&self, owner_repo: &str, commit_sha: &str) -> Result<String, String> {
        let url = git_url(&self.host, owner_repo, &format!("commits/{}", commit_sha))?;
        let resp = self.client.get(url.clone())
            .bearer_auth(&self.token)
            .send().await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("get_commit_tree failed ({}): {} {} (GET {})", commit_sha, status, text, url));
        }
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        let sha = tree_sha_from_value(&body);
        if sha.is_empty() {
            return Err(format!("get_commit_tree failed ({}): no tree sha in {}", commit_sha, body));
        }
        Ok(sha)
    }

    async fn create_blob(&self, owner_repo: &str, content_b64: &str) -> Result<String, String> {
        let url = git_url(&self.host, owner_repo, "blobs")?;
        let body = serde_json::json!({
            "content": content_b64,
            "encoding": "base64",
        });
        let resp = self.client.post(url.clone())
            .bearer_auth(&self.token)
            .json(&body)
            .send().await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("create_blob failed: {} {} (POST {})", status, text, url));
        }
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        let sha = body.get("sha").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if sha.is_empty() {
            return Err("create_blob failed: no sha".to_string());
        }
        Ok(sha)
    }

    async fn create_tree(&self, owner_repo: &str, base_tree: &str, path: &str, blob_sha: &str) -> Result<String, String> {
        let url = git_url(&self.host, owner_repo, "trees")?;
        let body = serde_json::json!({
            "base_tree": base_tree,
            "tree": [{
                "path": path,
                "mode": "100644",
                "type": "blob",
                "sha": blob_sha,
            }],
        });
        let resp = self.client.post(url.clone())
            .bearer_auth(&self.token)
            .json(&body)
            .send().await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("create_tree failed ({}): {} {} (POST {})", path, status, text, url));
        }
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        let sha = body.get("sha").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if sha.is_empty() {
            return Err(format!("create_tree failed ({}): no sha", path));
        }
        Ok(sha)
    }

    async fn create_commit(&self, owner_repo: &str, message: &str, tree_sha: &str, parent_sha: &str) -> Result<String, String> {
        let url = git_url(&self.host, owner_repo, "commits")?;
        let body = serde_json::json!({
            "message": message,
            "tree": tree_sha,
            "parents": [parent_sha],
        });
        let resp = self.client.post(url.clone())
            .bearer_auth(&self.token)
            .json(&body)
            .send().await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("create_commit failed: {} {} (POST {})", status, text, url));
        }
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        let sha = body.get("sha").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if sha.is_empty() {
            return Err("create_commit failed: no sha".to_string());
        }
        Ok(sha)
    }

    async fn update_ref(&self, owner_repo: &str, git_ref: &str, commit_sha: &str) -> Result<(), String> {
        let url = git_url(&self.host, owner_repo, &format!("refs/{}", git_ref))?;
        let body = serde_json::json!({
            "sha": commit_sha,
            "force": false,
        });
        let resp = self.client.patch(url.clone())
            .bearer_auth(&self.token)
            .json(&body)
            .send().await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("update_ref failed ({}): {} {} (PATCH {})", git_ref, status, text, url));
        }
        Ok(())
    }

    pub async fn get_file_content(&self, owner_repo: &str, path: &str) -> Result<Option<(String, String)>, String> {
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
            return Err(format!("get_file_content failed ({}): {} {} (GET {})", path, status, text, url));
        }
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        let sha = body.get("sha").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let inline = body.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let content = if !inline.trim().is_empty() {
            inline.trim_end().to_string()
        } else {
            let download_url = match body.get("download_url").and_then(|v| v.as_str()) {
                Some(u) if !u.is_empty() => u.to_string(),
                _ => return Err(format!("get_file_content: no inline content and no download_url for {}", path)),
            };
            let dl_resp = self.client.get(&download_url)
                .bearer_auth(&self.token)
                .send().await
                .map_err(|e| format!("get_file_content: download_url fetch failed ({}): {}", path, e))?;
            if !dl_resp.status().is_success() {
                let s = dl_resp.status();
                return Err(format!("get_file_content: download_url returned {} for {}", s, path));
            }
            let bytes = dl_resp.bytes().await
                .map_err(|e| format!("get_file_content: download_url body read failed ({}): {}", path, e))?;
            base64_encode_bytes(&bytes)
        };
        Ok(Some((content, sha)))
    }

    pub async fn delete_file(&self, owner_repo: &str, path: &str, sha: &str, message: &str) -> Result<(), String> {
        let url = contents_url(&self.host, owner_repo, path)?;
        let body = serde_json::json!({
            "sha": sha,
            "message": message,
        });
        let resp = self.client.delete(url.clone())
            .bearer_auth(&self.token)
            .json(&body)
            .send().await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let s = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("delete_file failed ({}): {} {} (DELETE {})", path, s, text, url));
        }
        Ok(())
    }

    pub async fn move_file(&self, owner_repo: &str, from_path: &str, to_path: &str, message: &str) -> Result<(), String> {
        let (content, sha) = match self.get_file_content(owner_repo, from_path).await? {
            Some(v) => v,
            None => return Err(format!("move_file: source not found: {}", from_path)),
        };
        self.create_file(owner_repo, to_path, &content, message).await?;
        self.delete_file(owner_repo, from_path, &sha, message).await?;
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

fn git_url(host: &str, owner_repo: &str, path: &str) -> Result<reqwest::Url, String> {
    let base = format!("{}/api/v1/repos/{}/git/", host, owner_repo);
    reqwest::Url::parse(&base)
        .and_then(|u| u.join(path))
        .map_err(|e| format!("invalid git URL ({}): {}", path, e))
}

fn ref_sha_from_value(value: &Value) -> String {
    if let Some(arr) = value.as_array() {
        return arr.iter()
            .find_map(|v| {
                let sha = ref_sha_from_value(v);
                if sha.is_empty() { None } else { Some(sha) }
            })
            .unwrap_or_default();
    }
    value.get("object")
        .and_then(|v| v.get("sha"))
        .and_then(|v| v.as_str())
        .or_else(|| value.get("sha").and_then(|v| v.as_str()))
        .or_else(|| value.get("id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string()
}

fn tree_sha_from_value(value: &Value) -> String {
    value.get("tree")
        .and_then(|v| v.get("sha").or_else(|| v.get("id")))
        .and_then(|v| v.as_str())
        .or_else(|| {
            value.get("commit")
                .and_then(|v| v.get("tree"))
                .and_then(|v| v.get("sha").or_else(|| v.get("id")))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string()
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
