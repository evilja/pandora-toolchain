use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, COOKIE};
use reqwest::redirect::Policy;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::libpnenv::core::get_pandora_env;
use crate::libpnenv::standard::ANIMECIX;

const ACIX_BASE: &str = "https://animecix.tv";
const BOOTSTRAP_PATH: &str = "/secure/translators";

pub const FANSUB_AKIRASUBS: i64 = 50;
pub const FANSUB_SOMESUBS: i64 = 218;

#[derive(Clone, Copy)]
pub enum MediaType {
    Series,
    Movies,
}

impl MediaType {
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaType::Series => "series",
            MediaType::Movies => "movies",
        }
    }
}

#[derive(Clone)]
struct Session {
    connect_sid: String,
    xsrf_token: String,
}

pub struct TmdbResolve {
    pub status: u16,
    pub acix_id: Option<i64>,
    pub body: Value,
}

#[derive(serde::Serialize)]
pub struct SearchHit {
    pub acix_id: i64,
    pub mal_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub name: String,
}

pub struct MixedUpload {
    pub extra: String,
    pub url: String,
    pub template: i64,
    pub title_id: i64,
    pub season_num: Option<i64>,
    pub episode_num: Option<i64>,
    pub quality: String,
    pub language: String,
    pub category: String,
    pub order: i64,
    pub machine_translate: bool,
    pub hardcode: bool,
}

impl MixedUpload {
    pub fn new(
        extra: String,
        url: String,
        template: i64,
        title_id: i64,
        season_num: Option<i64>,
        episode_num: Option<i64>,
    ) -> Self {
        Self {
            extra,
            url,
            template,
            title_id,
            season_num,
            episode_num,
            quality: "regular".to_string(),
            language: "tr".to_string(),
            category: "full".to_string(),
            order: 0,
            machine_translate: false,
            hardcode: false,
        }
    }
}

pub struct AnimeCix {
    token: String,
    client: Client,
    session: Mutex<Option<Session>>,
}

impl AnimeCix {
    pub fn new(bearer_token: String) -> Result<Self, String> {
        if bearer_token.is_empty() {
            return Err("AnimeCix bearer token is empty. Set `animecix` in env.pandora.".to_string());
        }
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static("pandora-toolchain"),
        );
        let client = Client::builder()
            .default_headers(headers)
            .redirect(Policy::none())
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(AnimeCix { token: bearer_token, client, session: Mutex::new(None) })
    }

    pub fn from_env() -> Result<Self, String> {
        let token = get_pandora_env().get(ANIMECIX).cloned().unwrap_or_default();
        Self::new(token)
    }

    pub fn with_session(bearer_token: String, connect_sid: String, xsrf_token: String) -> Result<Self, String> {
        let me = Self::new(bearer_token)?;
        *me.session.try_lock().unwrap() = Some(Session { connect_sid, xsrf_token });
        Ok(me)
    }

    async fn ensure_session(&self) -> Result<Session, String> {
        {
            let guard = self.session.lock().await;
            if let Some(s) = guard.as_ref() {
                return Ok(s.clone());
            }
        }
        let s = self.bootstrap_session().await?;
        *self.session.lock().await = Some(s.clone());
        Ok(s)
    }

    async fn bootstrap_session(&self) -> Result<Session, String> {
        let url = format!("{}{}", ACIX_BASE, BOOTSTRAP_PATH);
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(ACCEPT, "application/json, text/plain, */*")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let mut connect_sid = String::new();
        let mut xsrf_token = String::new();
        for cookie in resp.cookies() {
            match cookie.name() {
                "connect.sid" => connect_sid = cookie.value().to_string(),
                "XSRF-TOKEN" => xsrf_token = cookie.value().to_string(),
                _ => {}
            }
        }
        if connect_sid.is_empty() || xsrf_token.is_empty() {
            return Err(format!(
                "animecix session bootstrap returned no cookies (status {}, connect.sid found: {}, XSRF-TOKEN found: {})",
                status,
                !connect_sid.is_empty(),
                !xsrf_token.is_empty()
            ));
        }
        Ok(Session { connect_sid, xsrf_token })
    }

    fn cookie_header(&self, s: &Session) -> String {
        format!(
            "connect.sid={}; theme=Dark; null_cookie_notice=1; XSRF-TOKEN={}",
            s.connect_sid, s.xsrf_token
        )
    }

    async fn post_json(&self, url: &str, body: &Value) -> Result<(u16, Value), String> {
        let s = self.ensure_session().await?;
        let resp = self
            .client
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(COOKIE, self.cookie_header(&s))
            .header("X-XSRF-TOKEN", s.xsrf_token.as_str())
            .header(ACCEPT, "application/json, text/plain, */*")
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        let val: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        if !status.is_success() {
            return Err(format!("{} -> {} {}", url, status, val));
        }
        Ok((status.as_u16(), val))
    }

    pub async fn tmdb_to_acix(&self, tmdb_id: &str, media_type: MediaType) -> Result<TmdbResolve, String> {
        let body = serde_json::json!({
            "tmdbId": tmdb_id,
            "mediaType": media_type.as_str(),
        });
        let url = format!("{}/api/v1/media/import", ACIX_BASE);
        let (status, body) = self.post_json(&url, &body).await?;
        let acix_id = extract_id(&body);
        Ok(TmdbResolve { status, acix_id, body })
    }

    pub async fn multishare_mixed(&self, up: &MixedUpload) -> Result<Value, String> {
        let body = serde_json::json!({
            "extra": up.extra,
            "url": up.url,
            "quality": up.quality,
            "type": "multishare",
            "template": up.template,
            "category": up.category,
            "title_id": up.title_id,
            "season_num": up.season_num,
            "episode_num": up.episode_num,
            "language": up.language,
            "order": up.order,
            "machine_translate": up.machine_translate,
            "hardcode": up.hardcode,
        });
        let url = format!("{}/secure/multishare/mixed", ACIX_BASE);
        let (_status, val) = self.post_json(&url, &body).await?;
        Ok(val)
    }

    pub async fn translators(&self) -> Result<Value, String> {
        let s = self.ensure_session().await?;
        let url = format!("{}{}", ACIX_BASE, BOOTSTRAP_PATH);
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(COOKIE, self.cookie_header(&s))
            .header(ACCEPT, "application/json, text/plain, */*")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        let val: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        if !status.is_success() {
            return Err(format!("translators -> {} {}", status, val));
        }
        Ok(val)
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>, String> {
        let s = self.ensure_session().await?;
        let mut url = reqwest::Url::parse(ACIX_BASE).map_err(|e| e.to_string())?;
        url.path_segments_mut()
            .map_err(|_| "invalid base url".to_string())?
            .extend(["secure", "search", query]);
        url.query_pairs_mut().append_pair("limit", &limit.to_string());
        let resp = self
            .client
            .get(url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(COOKIE, self.cookie_header(&s))
            .header(ACCEPT, "application/json, text/plain, */*")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        let val: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        if !status.is_success() {
            return Err(format!("search -> {} {}", status, val));
        }
        let mut hits = Vec::new();
        for item in result_array(&val) {
            let acix_id = match item.get("id").and_then(|x| x.as_i64()) {
                Some(v) => v,
                None => continue,
            };
            hits.push(SearchHit {
                acix_id,
                mal_id: item.get("mal_id").and_then(|x| x.as_i64()),
                tmdb_id: item.get("tmdb_id").and_then(|x| x.as_i64()),
                name: pick_name(&item),
            });
        }
        Ok(hits)
    }

    pub async fn resolve_by_mal_id(&self, query: &str, mal_id: i64) -> Result<Option<SearchHit>, String> {
        let hits = self.search(query, 20).await?;
        Ok(hits.into_iter().find(|h| h.mal_id == Some(mal_id)))
    }
}

fn result_array(v: &Value) -> Vec<Value> {
    if let Some(a) = v.as_array() {
        return a.clone();
    }
    for k in ["results", "data", "animes", "titles", "hits"] {
        if let Some(a) = v.get(k).and_then(|x| x.as_array()) {
            return a.clone();
        }
    }
    Vec::new()
}

fn pick_name(item: &Value) -> String {
    for k in ["name", "title", "original_title", "title_tr", "title_en", "original_name"] {
        if let Some(s) = item.get(k).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    String::new()
}

fn extract_id(v: &Value) -> Option<i64> {
    for key in ["id", "title_id", "media_id", "acix_id"] {
        if let Some(n) = v.get(key).and_then(|x| x.as_i64()) {
            return Some(n);
        }
    }
    for parent in ["data", "media", "title", "result"] {
        if let Some(p) = v.get(parent) {
            for key in ["id", "title_id", "media_id"] {
                if let Some(n) = p.get(key).and_then(|x| x.as_i64()) {
                    return Some(n);
                }
            }
        }
    }
    None
}
