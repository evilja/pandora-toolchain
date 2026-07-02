use reqwest::multipart::{Form, Part};
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

const ANISUB_BASE: &str = "https://anisub.co";
pub const DEFAULT_FPS: &str = "23.976";

pub struct AniSub {
    token: String,
    client: Client,
}

#[derive(Clone)]
pub struct AnimeMatch {
    pub media_id: i64,
    pub title_turkish: String,
    pub title_english: String,
    pub title_japanese: String,
    pub url_slug: String,
}

pub struct UploadResult {
    pub subtitle_id: i64,
    pub filename: String,
}

impl AniSub {
    pub fn new(api_key: String) -> Result<Self, String> {
        if api_key.is_empty() {
            return Err("AniSub API key is empty. Set `anisub` via /touchapi.".to_string());
        }
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static("pandora-toolchain"),
        );
        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(AniSub { token: api_key, client })
    }

    pub async fn search_anime(&self, query: &str) -> Result<Vec<AnimeMatch>, String> {
        let url = format!("{}/api/anime/search", ANISUB_BASE);
        let resp = self.client.get(&url)
            .query(&[("q", query)])
            .send().await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("anime search failed: {} {}", status, text));
        }
        let json: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let arr = json.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let mut out = Vec::new();
        for item in arr {
            let media_id = item.get("media_id").and_then(|v| v.as_i64()).unwrap_or(0);
            if media_id == 0 {
                continue;
            }
            out.push(AnimeMatch {
                media_id,
                title_turkish: item.get("title_turkish").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                title_english: item.get("title_english").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                title_japanese: item.get("title_japanese").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                url_slug: item.get("url_slug").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            });
        }
        Ok(out)
    }

    pub async fn resolve_anilist(&self, name: &str) -> Result<Option<AnimeMatch>, String> {
        let results = self.search_anime(name).await?;
        if results.is_empty() {
            return Ok(None);
        }
        let norm = |s: &str| s.trim().to_lowercase();
        let target = norm(name);
        let exact = results.iter().find(|m| {
            norm(&m.title_english) == target
                || norm(&m.title_turkish) == target
                || norm(&m.title_japanese) == target
        });
        Ok(Some(exact.cloned().unwrap_or_else(|| results[0].clone())))
    }

    pub async fn upload_subtitle(
        &self,
        zip_bytes: Vec<u8>,
        zip_filename: &str,
        anilist_id: i64,
        release_name: &str,
        episode: u32,
        translator: &str,
        fps: &str,
    ) -> Result<UploadResult, String> {
        let url = format!("{}/api/admin/subtitle/upload", ANISUB_BASE);
        let part = Part::bytes(zip_bytes)
            .file_name(zip_filename.to_string())
            .mime_str("application/zip")
            .map_err(|e| e.to_string())?;
        let form = Form::new()
            .part("subtitle_file", part)
            .text("subtitle_anilist_id", anilist_id.to_string())
            .text("subtitle_release_name", release_name.to_string())
            .text("subtitle_language", "Türkçe")
            .text("subtitle_type", "Çeviri")
            .text("content_type", format!("Bölüm: {:02}", episode))
            .text("fps", fps.to_string())
            .text("format", "ass")
            .text("subtitle_translator", translator.to_string())
            .text("visibility", "public");
        let resp = self.client.post(&url)
            .bearer_auth(&self.token)
            .multipart(form)
            .send().await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("upload failed: {} {}", status, text));
        }
        let json: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if !json.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
            let msg = json.get("message").and_then(|v| v.as_str()).unwrap_or("unknown error");
            return Err(format!("upload rejected: {}", msg));
        }
        let data = json.get("data").cloned().unwrap_or(Value::Null);
        Ok(UploadResult {
            subtitle_id: data.get("subtitle_id").and_then(|v| v.as_i64()).unwrap_or(0),
            filename: data.get("filename").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        })
    }
}
