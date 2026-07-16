use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, COOKIE, ORIGIN, REFERER};
use reqwest::redirect::Policy;
use reqwest::Client;
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{ANIMECIX, ANIMECIX_EMAIL, ANIMECIX_PASSWORD};

const ACIX_BASE: &str = "https://animecix.tv";
const BOOTSTRAP_PATH: &str = "/secure/translators";
const LOGIN_PATH: &str = "/secure/auth/login";
const SESSION_PATH: &str = "DB/config/global/environment/animecix.session";
const SESSION_FALLBACK_TTL_SECS: u64 = 30 * 24 * 60 * 60;

static SESSION_ACCESS: Mutex<()> = Mutex::const_new(());

pub const FANSUB_AKIRASUBS: i64 = 50; //deconst
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

#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct Session {
    connect_sid: String,
    xsrf_token: String,
    account: Option<String>,
    expires_at: Option<u64>,
}

struct Credentials {
    email: String,
    password: String,
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
    credentials: Option<Credentials>,
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
        Ok(AnimeCix {
            token: bearer_token,
            client,
            credentials: None,
            session: Mutex::new(None),
        })
    }

    pub fn with_credentials(bearer_token: String, email: String, password: String) -> Result<Self, String> {
        if email.is_empty() {
            return Err("AnimeCix email is empty. Set `animecix_email` in env.pandora.".to_string());
        }
        if password.is_empty() {
            return Err("AnimeCix password is empty. Set `animecix_password` in env.pandora.".to_string());
        }
        let mut me = Self::new(bearer_token)?;
        me.credentials = Some(Credentials { email, password });
        Ok(me)
    }

    pub fn from_env() -> Result<Self, String> {
        let env = get_pandora_env();
        let token = env.get(ANIMECIX).cloned().unwrap_or_default();
        let email = env.get(ANIMECIX_EMAIL).cloned().unwrap_or_default();
        let password = env.get(ANIMECIX_PASSWORD).cloned().unwrap_or_default();
        Self::with_credentials(token, email, password)
    }

    pub fn with_session(bearer_token: String, connect_sid: String, xsrf_token: String) -> Result<Self, String> {
        let me = Self::new(bearer_token)?;
        *me.session.try_lock().unwrap() = Some(Session {
            connect_sid,
            xsrf_token,
            account: None,
            expires_at: None,
        });
        Ok(me)
    }

    async fn ensure_session(&self) -> Result<Session, String> {
        let mut guard = self.session.lock().await;
        if let Some(s) = guard.as_ref() {
            return Ok(s.clone());
        }
        let credentials = self.credentials.as_ref().ok_or_else(|| {
            "AnimeCix login credentials are unavailable; set `animecix_email` and `animecix_password` in env.pandora."
                .to_string()
        })?;
        let _access = SESSION_ACCESS.lock().await;
        if let Some(s) = load_cached_session(&credentials.email).await {
            *guard = Some(s.clone());
            return Ok(s);
        }
        let s = self.bootstrap_session().await?;
        save_cached_session(&s).await?;
        *guard = Some(s.clone());
        Ok(s)
    }

    async fn clear_session(&self, failed: &Session) {
        *self.session.lock().await = None;
        let _access = SESSION_ACCESS.lock().await;
        if let Some(cached) = load_cached_session_any().await {
            if cached.connect_sid == failed.connect_sid {
                tokio::fs::remove_file(SESSION_PATH).await.ok();
            }
        }
    }

    async fn bootstrap_session(&self) -> Result<Session, String> {
        let credentials = self.credentials.as_ref().ok_or_else(|| {
            "AnimeCix login credentials are unavailable; set `animecix_email` and `animecix_password` in env.pandora."
                .to_string()
        })?;
        let bootstrap_url = format!("{}{}", ACIX_BASE, BOOTSTRAP_PATH);
        let bootstrap_resp = self
            .client
            .get(&bootstrap_url)
            .header(ACCEPT, "application/json, text/plain, */*")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let bootstrap_status = bootstrap_resp.status();
        let mut xsrf_token = String::new();
        for cookie in bootstrap_resp.cookies() {
            if cookie.name() == "XSRF-TOKEN" {
                xsrf_token = cookie.value().to_string();
            }
        }
        if !bootstrap_status.is_success() || xsrf_token.is_empty() {
            return Err(format!(
                "animecix XSRF bootstrap failed (status {}, XSRF-TOKEN found: {})",
                bootstrap_status,
                !xsrf_token.is_empty()
            ));
        }

        let login_url = format!("{}{}", ACIX_BASE, LOGIN_PATH);
        let login_body = serde_json::json!({
            "email": credentials.email,
            "password": credentials.password,
            "remember": true,
        });
        let login_resp = self
            .client
            .post(&login_url)
            .header(COOKIE, format!("XSRF-TOKEN={}", xsrf_token))
            .header("X-XSRF-TOKEN", percent_decode(&xsrf_token))
            .header(ACCEPT, "application/json, text/plain, */*")
            .header(CONTENT_TYPE, "application/json")
            .header(ORIGIN, ACIX_BASE)
            .header(REFERER, format!("{}/login", ACIX_BASE))
            .body(login_body.to_string())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let login_status = login_resp.status();
        let mut connect_sid = String::new();
        let mut expires_at = None;
        for cookie in login_resp.cookies() {
            match cookie.name() {
                "connect.sid" => {
                    connect_sid = cookie.value().to_string();
                    expires_at = cookie_expiry(&cookie);
                }
                "XSRF-TOKEN" => xsrf_token = cookie.value().to_string(),
                _ => {}
            }
        }
        let login_text = login_resp.text().await.map_err(|e| e.to_string())?;
        if !login_status.is_success() {
            let val: Value = serde_json::from_str(&login_text).unwrap_or(Value::String(login_text));
            return Err(format!("animecix login -> {} {}", login_status, val));
        }
        if connect_sid.is_empty() {
            return Err(format!(
                "animecix login returned no connect.sid cookie (status {})",
                login_status
            ));
        }
        Ok(Session {
            connect_sid,
            xsrf_token,
            account: Some(credentials.email.clone()),
            expires_at: Some(expires_at.unwrap_or_else(|| now_secs() + SESSION_FALLBACK_TTL_SECS)),
        })
    }

    fn cookie_header(&self, s: &Session) -> String {
        format!(
            "connect.sid={}; theme=Dark; null_cookie_notice=1; XSRF-TOKEN={}",
            s.connect_sid, s.xsrf_token
        )
    }

    async fn post_json(&self, url: &str, body: &Value) -> Result<(u16, Value), String> {
        for attempt in 0..2 {
            let s = self.ensure_session().await?;
            let resp = self
                .client
                .post(url)
                .header(AUTHORIZATION, format!("Bearer {}", self.token))
                .header(COOKIE, self.cookie_header(&s))
                .header("X-XSRF-TOKEN", percent_decode(&s.xsrf_token))
                .header(ACCEPT, "application/json, text/plain, */*")
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string())
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let text = resp.text().await.map_err(|e| e.to_string())?;
            let val: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
            if attempt == 0 && matches!(status.as_u16(), 401 | 403 | 419) {
                self.clear_session(&s).await;
                continue;
            }
            if !status.is_success() {
                return Err(format!("{} -> {} {}", url, status, val));
            }
            return Ok((status.as_u16(), val));
        }
        unreachable!()
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

async fn load_cached_session(account: &str) -> Option<Session> {
    let session = load_cached_session_any().await?;
    if !session_reusable(&session, account, now_secs()) {
        return None;
    }
    Some(session)
}

fn session_reusable(session: &Session, account: &str, now: u64) -> bool {
    session.account.as_deref() == Some(account)
        && session.expires_at.unwrap_or_default() > now + 60
        && !session.connect_sid.is_empty()
        && !session.xsrf_token.is_empty()
}

async fn load_cached_session_any() -> Option<Session> {
    let text = tokio::fs::read_to_string(SESSION_PATH).await.ok()?;
    serde_json::from_str(&text).ok()
}

async fn save_cached_session(session: &Session) -> Result<(), String> {
    let path = std::path::Path::new(SESSION_PATH);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
    }
    let temp_path = format!("{}.tmp", SESSION_PATH);
    tokio::fs::remove_file(&temp_path).await.ok();
    let mut options = tokio::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temp_path).await.map_err(|e| e.to_string())?;
    let json = serde_json::to_vec(session).map_err(|e| e.to_string())?;
    file.write_all(&json).await.map_err(|e| e.to_string())?;
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    tokio::fs::rename(&temp_path, SESSION_PATH).await.map_err(|e| e.to_string())?;
    Ok(())
}

fn cookie_expiry(cookie: &reqwest::cookie::Cookie<'_>) -> Option<u64> {
    cookie
        .expires()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .or_else(|| cookie.max_age().map(|duration| now_secs() + duration.as_secs()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push((high << 4) | low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_string())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
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

#[cfg(test)]
mod tests {
    use super::{percent_decode, session_reusable, Session};

    #[test]
    fn decodes_percent_encoded_xsrf_tokens() {
        assert_eq!(percent_decode("abc%2Fdef%2Bghi%3D"), "abc/def+ghi=");
        assert_eq!(percent_decode("plain-token"), "plain-token");
        assert_eq!(percent_decode("bad%2-token"), "bad%2-token");
    }

    #[test]
    fn reuses_only_matching_unexpired_sessions() {
        let session = Session {
            connect_sid: "sid".to_string(),
            xsrf_token: "xsrf".to_string(),
            account: Some("user@example.com".to_string()),
            expires_at: Some(10_000),
        };
        assert!(session_reusable(&session, "user@example.com", 1_000));
        assert!(!session_reusable(&session, "other@example.com", 1_000));
        assert!(!session_reusable(&session, "user@example.com", 9_950));
    }
}
