use capella::animecix::{AnimeciXClient, TranslatorTemplate};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{ANIMECIX, ANIMECIX_EMAIL, ANIMECIX_PASSWORD};
use crate::lib::http::directory::{self, MemoryCache};

pub use capella::animecix::{
    MediaType, TmdbImportResult as TmdbResolve, VideoPayload as MixedUpload,
};

// What Capella's own `resolve_mal_id` asks for, kept the same so an alias search sees the same page
// of results a single-title resolve would have.
const ALIAS_SEARCH_LIMIT: u32 = 20;
const SESSION_PATH: &str = "DB/config/global/environment/animecix.session";
const SESSION_FALLBACK_TTL_SECS: u64 = 30 * 24 * 60 * 60;
const DIRECTORY_SITE: &str = "animecix";

static SESSION_ACCESS: Mutex<()> = Mutex::const_new(());
static FANSUB_CACHE: MemoryCache<Vec<FansubTemplate>> = Mutex::const_new(None);

#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct Session {
    connect_sid: String,
    xsrf_token: String,
    account: Option<String>,
    expires_at: Option<u64>,
}

#[derive(Clone)]
struct Credentials {
    token: String,
    email: String,
    password: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FansubTemplate {
    pub id: i64,
    pub name: String,
    pub translator: String,
}

impl FansubTemplate {
    pub fn display_name(&self) -> String {
        if self.translator.is_empty() || self.translator.eq_ignore_ascii_case(&self.name) {
            self.name.clone()
        } else {
            format!("{} — {}", self.name, self.translator)
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SearchHit {
    pub acix_id: i64,
    pub mal_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub name: String,
}

// The outcome of searching AnimeciX under every title an anime is known by. The two catalogs Pandora
// publishes to disagree on the id often enough — one AnimeciX entry covering several MyAnimeList
// seasons, or an entry recording an id that belongs to something else — that "AnimeciX does not
// carry this anime" and "AnimeciX carries it under another id" have to be told apart in a report.
pub enum AliasMatch {
    Found(SearchHit),
    NotFound(Vec<SearchHit>),
}

pub struct AnimeCix {
    client: Mutex<AnimeciXClient>,
    credentials: Option<Credentials>,
    initialized: Mutex<bool>,
}

// Capella owns the HTTP contract and response parsing; Pandora keeps only the autocomplete
// directory, served from disk so a keystroke never waits on AnimeciX. See `lib::http::directory`.
pub async fn fetch_fansub_templates() -> Result<Vec<FansubTemplate>, String> {
    directory::cached(DIRECTORY_SITE, &FANSUB_CACHE, fetch_templates_uncached).await
}

pub async fn refresh_fansub_templates() -> Result<Vec<FansubTemplate>, String> {
    directory::refresh_now(DIRECTORY_SITE, &FANSUB_CACHE, fetch_templates_uncached).await
}

// An empty directory is an error rather than a cached result, so a transient bad response cannot
// overwrite a good copy on disk with nothing.
async fn fetch_templates_uncached() -> Result<Vec<FansubTemplate>, String> {
    let templates = AnimeCix::from_token_env()?.translator_templates().await?;
    if templates.is_empty() {
        return Err("AnimeciX fansub directory returned no templates".to_string());
    }
    Ok(templates)
}

impl AnimeCix {
    pub fn new(bearer_token: String) -> Result<Self, String> {
        let client = AnimeciXClient::new(bearer_token).map_err(|e| e.to_string())?;
        Ok(Self {
            client: Mutex::new(client),
            credentials: None,
            initialized: Mutex::new(true),
        })
    }

    pub fn with_credentials(
        bearer_token: String,
        email: String,
        password: String,
    ) -> Result<Self, String> {
        if email.is_empty() {
            return Err(
                "AnimeCix email is empty. Set `animecix_email` in env.pandora.".to_string(),
            );
        }
        if password.is_empty() {
            return Err(
                "AnimeCix password is empty. Set `animecix_password` in env.pandora.".to_string(),
            );
        }
        let credentials = Credentials {
            token: bearer_token,
            email,
            password,
        };
        let client = authenticated_client(&credentials, None)?;
        Ok(Self {
            client: Mutex::new(client),
            credentials: Some(credentials),
            initialized: Mutex::new(false),
        })
    }

    pub fn from_env() -> Result<Self, String> {
        let env = get_pandora_env();
        let token = env.get(ANIMECIX).cloned().unwrap_or_default();
        let email = env.get(ANIMECIX_EMAIL).cloned().unwrap_or_default();
        let password = env.get(ANIMECIX_PASSWORD).cloned().unwrap_or_default();
        Self::with_credentials(token, email, password)
    }

    pub fn from_token_env() -> Result<Self, String> {
        let env = get_pandora_env();
        Self::new(env.get(ANIMECIX).cloned().unwrap_or_default())
    }

    pub fn with_session(
        bearer_token: String,
        connect_sid: String,
        xsrf_token: String,
    ) -> Result<Self, String> {
        let client = AnimeciXClient::with_session(bearer_token, connect_sid, xsrf_token)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client: Mutex::new(client),
            credentials: None,
            initialized: Mutex::new(true),
        })
    }

    async fn plain_client(&self) -> AnimeciXClient {
        self.client.lock().await.clone()
    }

    // Session persistence remains Pandora-owned. The first privileged request
    // serializes cache loading/login across clients, then Capella owns refresh
    // and the authenticated request lifecycle.
    async fn privileged_client(&self) -> Result<AnimeciXClient, String> {
        let credentials = match self.credentials.as_ref() {
            Some(credentials) => credentials,
            None => return Ok(self.plain_client().await),
        };
        let mut initialized = self.initialized.lock().await;
        if *initialized {
            return Ok(self.plain_client().await);
        }

        let _access = SESSION_ACCESS.lock().await;
        let cached = load_cached_session(&credentials.email).await;
        let client = authenticated_client(credentials, cached.as_ref())?;
        let tokens = client.session_tokens().await.map_err(|e| e.to_string())?;
        save_cached_session(&Session {
            connect_sid: tokens.connect_sid,
            xsrf_token: tokens.xsrf_token,
            account: Some(credentials.email.clone()),
            expires_at: Some(now_secs() + SESSION_FALLBACK_TTL_SECS),
        })
        .await?;
        *self.client.lock().await = client.clone();
        *initialized = true;
        Ok(client)
    }

    async fn persist_session(&self, client: &AnimeciXClient) -> Result<(), String> {
        let credentials = match self.credentials.as_ref() {
            Some(credentials) => credentials,
            None => return Ok(()),
        };
        let tokens = client.session_tokens().await.map_err(|e| e.to_string())?;
        let _access = SESSION_ACCESS.lock().await;
        save_cached_session(&Session {
            connect_sid: tokens.connect_sid,
            xsrf_token: tokens.xsrf_token,
            account: Some(credentials.email.clone()),
            expires_at: Some(now_secs() + SESSION_FALLBACK_TTL_SECS),
        })
        .await
    }

    async fn finish_privileged<T>(
        &self,
        client: &AnimeciXClient,
        result: Result<T, capella::animecix::Error>,
    ) -> Result<T, String> {
        let persisted = self.persist_session(client).await;
        match result {
            Ok(value) => {
                persisted?;
                Ok(value)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    pub async fn tmdb_to_acix(
        &self,
        tmdb_id: &str,
        media_type: MediaType,
    ) -> Result<TmdbResolve, String> {
        let client = self.privileged_client().await?;
        let result = client.import_tmdb(tmdb_id, media_type).await;
        self.finish_privileged(&client, result).await
    }

    pub async fn multishare_mixed(&self, upload: &MixedUpload) -> Result<Value, String> {
        let client = self.privileged_client().await?;
        let result = client.publish_multishare(upload).await;
        self.finish_privileged(&client, result)
            .await
            .map(|response| response.body)
    }

    pub async fn multiple(&self, upload: &MixedUpload, links: &[String]) -> Result<Value, String> {
        let client = self.privileged_client().await?;
        let result = client.publish_multiple(upload, links).await;
        self.finish_privileged(&client, result)
            .await
            .map(|response| response.body)
    }

    async fn translator_templates(&self) -> Result<Vec<FansubTemplate>, String> {
        self.plain_client()
            .await
            .translators()
            .await
            .map(|templates| templates.into_iter().map(fansub_template).collect())
            .map_err(|e| e.to_string())
    }

    pub async fn translators(&self) -> Result<Value, String> {
        self.plain_client()
            .await
            .translators()
            .await
            .map(|templates| {
                Value::Array(templates.into_iter().map(|template| template.raw).collect())
            })
            .map_err(|e| e.to_string())
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>, String> {
        self.plain_client()
            .await
            .search(query, limit)
            .await
            .map(|hits| {
                hits.into_iter()
                    .map(|hit| SearchHit {
                        acix_id: hit.acix_id,
                        mal_id: hit.mal_id,
                        tmdb_id: hit.tmdb_id,
                        name: hit.name,
                    })
                    .collect()
            })
            .map_err(|e| e.to_string())
    }

    pub async fn resolve_by_mal_id(
        &self,
        query: &str,
        mal_id: i64,
    ) -> Result<Option<SearchHit>, String> {
        self.plain_client()
            .await
            .resolve_mal_id(query, mal_id)
            .await
            .map(|hit| {
                hit.map(|hit| SearchHit {
                    acix_id: hit.acix_id,
                    mal_id: hit.mal_id,
                    tmdb_id: hit.tmdb_id,
                    name: hit.name,
                })
            })
            .map_err(|e| e.to_string())
    }

    // AnimeciX reaches a MyAnimeList id only through its own title search, so an anime named by
    // another catalog is searched under each of its aliases until one of them returns the id. A
    // failed request is not a missing entry: the error is only reported when no alias got an answer
    // at all, so one bad search cannot report a title AnimeciX simply does not carry as an outage.
    pub async fn resolve_by_mal_id_aliases(
        &self,
        titles: &[String],
        mal_id: i64,
    ) -> Result<AliasMatch, String> {
        let mut candidates = Vec::new();
        let mut searched = false;
        let mut last_error = None;
        for title in titles {
            match self.search(title, ALIAS_SEARCH_LIMIT).await {
                Ok(hits) => {
                    searched = true;
                    if let Some(hit) = take_mal_match(hits, mal_id, &mut candidates) {
                        return Ok(AliasMatch::Found(hit));
                    }
                }
                Err(e) => last_error = Some(e),
            }
        }
        match last_error {
            Some(e) if !searched => Err(e),
            _ => Ok(AliasMatch::NotFound(candidates)),
        }
    }
}

fn authenticated_client(
    credentials: &Credentials,
    session: Option<&Session>,
) -> Result<AnimeciXClient, String> {
    let mut builder = AnimeciXClient::builder(credentials.token.clone())
        .credentials(credentials.email.clone(), credentials.password.clone());
    if let Some(session) = session {
        builder = builder.session(session.connect_sid.clone(), session.xsrf_token.clone());
    }
    builder.build().map_err(|e| e.to_string())
}

// Scans one alias search for the requested id. Everything else is kept as a candidate — deduplicated
// across aliases, which overlap heavily — so a miss can still report what AnimeciX does carry.
fn take_mal_match(
    hits: Vec<SearchHit>,
    mal_id: i64,
    candidates: &mut Vec<SearchHit>,
) -> Option<SearchHit> {
    let mut found = None;
    for hit in hits {
        if found.is_none() && hit.mal_id == Some(mal_id) {
            found = Some(hit);
            continue;
        }
        if !candidates.iter().any(|kept| kept.acix_id == hit.acix_id) {
            candidates.push(hit);
        }
    }
    found
}

fn fansub_template(template: TranslatorTemplate) -> FansubTemplate {
    FansubTemplate {
        id: template.id,
        name: template.name,
        translator: template.translator,
    }
}

async fn load_cached_session(account: &str) -> Option<Session> {
    let text = tokio::fs::read_to_string(SESSION_PATH).await.ok()?;
    let session: Session = serde_json::from_str(&text).ok()?;
    if session.account.as_deref() == Some(account)
        && session.expires_at.unwrap_or_default() > now_secs() + 60
        && !session.connect_sid.is_empty()
        && !session.xsrf_token.is_empty()
    {
        Some(session)
    } else {
        None
    }
}

async fn save_cached_session(session: &Session) -> Result<(), String> {
    let path = std::path::Path::new(SESSION_PATH);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
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
    tokio::fs::rename(&temp_path, SESSION_PATH)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{take_mal_match, SearchHit};

    fn hit(acix_id: i64, mal_id: Option<i64>, name: &str) -> SearchHit {
        SearchHit {
            acix_id,
            mal_id,
            tmdb_id: None,
            name: name.to_string(),
        }
    }

    #[test]
    fn the_requested_id_wins_and_everything_else_is_kept_as_a_candidate() {
        let mut candidates = Vec::new();
        let found = take_mal_match(
            vec![
                hit(1, Some(43608), "Kaguya-sama wa Kokurasetai"),
                hit(2, Some(37999), "Kaguya-sama S1"),
                hit(3, None, "Unlinked"),
            ],
            37999,
            &mut candidates,
        );
        assert_eq!(found.unwrap().acix_id, 2);
        assert_eq!(
            candidates.iter().map(|hit| hit.acix_id).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn aliases_that_return_the_same_entries_do_not_repeat_them() {
        let mut candidates = Vec::new();
        // The romaji and English searches of one anime overlap almost completely, so the report must
        // not name the same AnimeciX entry once per alias.
        assert!(take_mal_match(vec![hit(1, Some(3006), "SPY×FAMILY")], 50265, &mut candidates).is_none());
        assert!(take_mal_match(
            vec![hit(1, Some(3006), "SPY×FAMILY"), hit(2, Some(57435), "CODE: White")],
            50265,
            &mut candidates
        )
        .is_none());
        assert_eq!(
            candidates.iter().map(|hit| hit.acix_id).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
}
