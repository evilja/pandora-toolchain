use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Path, Query, Request, State},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc::Sender};

use crate::pnworker::core::{HalfJob, Job, JobClass, JobType, KeepRequest, KeycodeRequest, Preset, SmartcodeDriveName, Stage};
use crate::pnworker::link::spec::NodePurpose;
use crate::pnworker::server_effects::preset_from_name;
use crate::pnworker::acix::confirm_acix;
use crate::pnworker::batch::batch_job_for_token;
use crate::lib::http::acix::{AnimeCix, MediaType, MixedUpload};
use crate::lib::db::core::{JobDb, JobStatus};
use crate::lib::git::{
    attach_repo, destruct_repo, detach_channel, init_repo, list_attachments, set_source,
    smartcode_merge, Credits, RepoOutcome,
};
use crate::lib::p2p::nyaaise::nyaaise;
use crate::lib::p2p::nyaaise::TorrentType;
use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{
    API_AUTHOR_ID, API_HOST, API_RATE_LIMIT, API_RATE_WINDOW_SECS, API_TOKENS_PATH,
};

pub(super) const STUDIO_AUDIO_FILE_LIMIT: usize = 50 * 1024 * 1024;
const STUDIO_AUDIO_REQUEST_LIMIT: usize = 70 * 1024 * 1024;
const API_REQUEST_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct AppState {
    pub(super) tx: Sender<JobClass>,
    pub(super) db: Arc<JobDb>,
    pub(super) api_author: u64,
    rate_limiter: Arc<ApiRateLimiter>,
}

#[derive(Clone)]
pub(super) struct ApiAuth {
    pub(super) local_server_id: Option<u64>,
    // Set by a `<token>|link|<node>` line. Such a token authorises the link routes and nothing
    // else: a node cannot submit jobs, read another job's logs, or touch git.
    pub(super) link_node: Option<String>,
    // The optional fourth field of that line, `<token>|link|<node>|gpu`. What a node is allowed to
    // run is decided here rather than by the node, so a machine cannot put itself in the way of
    // work it has no hardware for.
    pub(super) link_purpose: NodePurpose,
    // The token as presented. Request-scoped and never logged or serialised; the self-revoke route
    // needs the value itself, since the token file stores tokens and not their hashes.
    token: String,
    token_hash: String,
    label: Option<String>,
}

pub(super) fn presented_token(auth: &ApiAuth) -> Option<String> {
    if auth.token.is_empty() {
        None
    } else {
        Some(auth.token.clone())
    }
}

struct ApiRateLimiter {
    buckets: Mutex<std::collections::HashMap<String, RateBucket>>,
    max: u32,
    window: Duration,
}

struct RateBucket {
    start: Instant,
    count: u32,
}

impl ApiRateLimiter {
    fn new(max: u32, window: Duration) -> Self {
        Self {
            buckets: Mutex::new(std::collections::HashMap::new()),
            max: max.max(1),
            window,
        }
    }

    async fn check(&self, key: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().await;
        buckets.retain(|_, bucket| now.duration_since(bucket.start) < self.window);
        let bucket = buckets.entry(key.to_string()).or_insert(RateBucket {
            start: now,
            count: 0,
        });
        let elapsed = now.duration_since(bucket.start);
        if elapsed >= self.window {
            bucket.start = now;
            bucket.count = 0;
        }
        if bucket.count >= self.max {
            return Err(self.window.saturating_sub(elapsed).as_secs().max(1));
        }
        bucket.count += 1;
        Ok(())
    }
}

pub async fn serve(tx: Sender<JobClass>, port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let env = get_pandora_env();
    let db = Arc::new(JobDb::new().await?);
    db.init_schema().await?;
    db.migrate().await?;
    let api_author = env
        .get(API_AUTHOR_ID)
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    // Default to all interfaces so the public IP can reach it directly (no reverse
    // proxy required). Set `api_host` to `127.0.0.1` to keep it loopback-only.
    let host = env
        .get(API_HOST)
        .and_then(|s| s.trim().parse::<IpAddr>().ok())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let rate_limit = env
        .get(API_RATE_LIMIT)
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(1000);
    let rate_window = env
        .get(API_RATE_WINDOW_SECS)
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(60);
    let state = AppState {
        tx,
        db,
        api_author,
        rate_limiter: Arc::new(ApiRateLimiter::new(
            rate_limit,
            Duration::from_secs(rate_window.max(1)),
        )),
    };

    let protected = Router::new()
        .route("/jobs", get(list_jobs))
        .route("/jobs/:id", get(get_job))
        .route("/jobs/encode", post(submit_encode))
        .route("/jobs/backup", post(submit_backup))
        .route("/jobs/probe", post(submit_probe))
        .route("/jobs/pancode", post(submit_pancode))
        .route("/jobs/gitcode", post(submit_gitcode))
        .route("/jobs/keycode", post(submit_keycode))
        .route("/jobs/:id/cancel", post(cancel_job))
        .route("/workers", get(super::workers::workers))
        .route("/token/revoke", post(super::token::revoke))
        .route("/jobs/:id/logs", get(super::logs::list_logs))
        .route("/jobs/:id/logs.zip", get(super::logs::download_logs))
        .route("/jobs/:id/logs/:name", get(super::logs::read_log))
        .route("/jobs/:id/acix/confirm", post(acix_confirm))
        .route("/studios", get(super::studio::list).post(super::studio::create))
        .route("/studios/current", get(super::studio::current))
        .route("/studios/current/disown", post(super::studio::disown))
        .route("/studios/current/keywords", post(super::studio::replace_keywords))
        .route(
            "/studios/current/tracks",
            post(super::studio::add_track)
                .layer(DefaultBodyLimit::max(STUDIO_AUDIO_REQUEST_LIMIT)),
        )
        .route("/studios/current/media/sources/:source_index", get(super::studio::source_media))
        .route("/studios/current/media/tracks/:track_id", get(super::studio::track_media))
        .route("/studios/current/tracks/:track_id/edit", post(super::studio::edit_track))
        .route("/studios/current/tracks/:track_id/move", post(super::studio::move_track))
        .route("/studios/current/tracks/:track_id/cut", post(super::studio::cut_track))
        .route("/studios/current/tracks/:track_id/remove", post(super::studio::remove_track))
        .route("/studios/current/timeline", post(super::studio::timeline))
        .route("/studios/current/preview", post(super::studio::preview))
        .route("/studios/current/render", post(super::studio::render))
        .route("/studios/:id", get(super::studio::details))
        .route("/studios/:id/switch", post(super::studio::switch))
        .route("/studios/:id/reown", post(super::studio::reown))
        .route("/git/attachments", get(git_attachments))
        .route("/git/channels", get(git_channels))
        .route("/git/readmebase", get(git_readmebase).post(git_readmebase_set))
        .route("/git/init", post(git_init))
        .route("/git/attach", post(git_attach))
        .route("/git/source", post(git_source))
        .route("/git/detach", post(git_detach))
        .route("/git/destruct", post(git_destruct))
        .route("/git/smartcode", post(git_smartcode))
        .route("/link/register", post(super::link::register))
        .route("/link/lease", get(super::link::lease))
        .route("/link/lease/:id/renew", post(super::link::renew))
        .route("/link/lease/:id/result", post(super::link::result))
        .route(
            "/link/lease/:id/output",
            // A finished episode, not a JSON payload: the 8 MiB request cap that protects every
            // other route would reject it outright. The body is streamed straight to disk.
            axum::routing::put(super::link::output).layer(DefaultBodyLimit::disable()),
        )
        .route("/link/release", get(super::link::release))
        .route("/link/assets/manifest", get(super::link::assets_manifest))
        .route("/link/assets/:hash", get(super::link::asset))
        .route("/gitsync", post(gitsync))
        .route("/acix/search", post(acix_search))
        .route("/acix/tmdb", post(acix_tmdb))
        .route("/acix/translators", get(acix_translators))
        .route("/acix/publish", post(acix_publish))
        .route(
            "/trace",
            post(super::trace::run_trace)
                .layer(DefaultBodyLimit::max(crate::kagami_trace::MAX_ENCODED_BYTES)),
        )
        .route(
            "/trace/ass",
            post(super::trace::export_ass)
                .layer(DefaultBodyLimit::max(super::trace::ASS_REQUEST_LIMIT)),
        )
        .layer(DefaultBodyLimit::max(API_REQUEST_LIMIT))
        .layer(middleware::from_fn_with_state(state.clone(), auth));

    let app = Router::new()
        // The console is one shell with four in-page views, so `/`, `/jobs`, `/encode` and
        // `/settings` are the same document picking its view from the path.
        .route("/", get(index))
        .route("/jobs", get(index))
        .route("/encode", get(index))
        .route("/settings", get(index))
        .route("/git", get(git_console))
        .route("/studio", get(studio_console))
        .route("/trace", get(super::trace::index))
        .route("/console.css", get(console_css))
        .route("/console.js", get(console_js))
        .route("/studio-sw.js", get(studio_service_worker))
        .route("/favicon", get(favicon))
        .route("/favicon.ico", get(favicon))
        .route("/health", get(health))
        .route("/batch/:token", get(batch_page))
        .route("/batch/:token/output", get(batch_output))
        .route(
            "/lumiere/v1/files/:token/:filename",
            get(crate::lumiere_broker::serve_transfer),
        )
        .route(
            "/lumiere/v1/hls/:token/*resource",
            get(crate::lumiere_broker::serve_hls),
        )
        .nest("/api/v1", protected)
        .with_state(state);

    let addr = SocketAddr::new(host, port);
    let listener = TcpListener::bind(addr).await?;
    println!("[Pandora API] serving console + API on http://{}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}

const INDEX_HTML: &str = include_str!("../../../../web/index.html");
const GIT_HTML: &str = include_str!("../../../../web/git.html");
const STUDIO_HTML: &str = include_str!("../../../../web/studio.html");
const STUDIO_SERVICE_WORKER: &str = include_str!("../../../../web/studio-sw.js");
// The design system and the shell that draws the rail and topbar. Every console page links
// these, so a change here reaches all of them at once instead of being copied four times.
const SHELL_CSS: &str = include_str!("../../../../web/shell.css");
const SHELL_JS: &str = include_str!("../../../../web/shell.js");

async fn console_css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        SHELL_CSS,
    )
        .into_response()
}

async fn console_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        SHELL_JS,
    )
        .into_response()
}

async fn index() -> axum::response::Html<&'static str> {
    axum::response::Html(INDEX_HTML)
}

async fn git_console() -> axum::response::Html<&'static str> {
    axum::response::Html(GIT_HTML)
}

async fn studio_console() -> axum::response::Html<&'static str> {
    axum::response::Html(STUDIO_HTML)
}

async fn studio_service_worker() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        STUDIO_SERVICE_WORKER,
    ).into_response()
}

const BATCH_HTML: &str = include_str!("../../../../web/batch.html");

// The batch page and its data are authorized by the token in the path, the same capability shape
// the Lumiere transfer routes use: the link is posted to Discord, where nobody has an API token.
async fn batch_page(Path(token): Path<String>) -> Response {
    if batch_job_for_token(&token).await.is_none() {
        return (StatusCode::NOT_FOUND, "no such batch").into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        BATCH_HTML,
    )
        .into_response()
}

// Cloudflare caches HTML far more eagerly than it caches XHR, so the page ships static and asks
// for this separately with no-store rather than being rendered server-side.
async fn batch_output(State(st): State<AppState>, Path(token): Path<String>) -> Response {
    let Some(job_id) = batch_job_for_token(&token).await else {
        return (StatusCode::NOT_FOUND, "no such batch").into_response();
    };
    let parent = match st.db.get_job(job_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return (StatusCode::NOT_FOUND, "no such batch").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let progress: serde_json::Value = parent
        .progress
        .as_deref()
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_else(|| json!({}));
    let entries = progress
        .get("entries")
        .and_then(|value| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let child_id = |entry: &serde_json::Value| {
        entry
            .get("job_id")
            .and_then(|value| value.as_str())
            .and_then(|value| value.parse::<u64>().ok())
    };
    // Every episode's row in one query: a lost child still renders as an entry with null stage and
    // progress, exactly as it did when each one was fetched on its own.
    let children = st
        .db
        .get_jobs_by_ids(&entries.iter().filter_map(child_id).collect::<Vec<_>>())
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| (row.job_id, row))
                .collect::<std::collections::HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut episodes = Vec::with_capacity(entries.len());
    for entry in entries {
        let child = child_id(entry).and_then(|id| children.get(&(id as i64)));
        episodes.push(json!({
            "index": entry.get("index"),
            "label": entry.get("label"),
            "subtitle": entry.get("subtitle"),
            "job_id": entry.get("job_id"),
            "stage": child.map(|row| crate::lib::db::core::stage_label(row.stage)),
            "worker": child.map(|row| row.worker.clone()),
            "progress": child
                .and_then(|row| row.progress.as_deref())
                .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok()),
            "links": child
                .and_then(|row| row.uploaded_links.as_deref())
                .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok()),
        }));
    }
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "job_id": parent.job_id.to_string(),
            "stage": crate::lib::db::core::stage_label(parent.stage),
            "source": parent.link,
            "total": progress.get("total"),
            "finished": progress.get("finished"),
            "failed": progress.get("failed"),
            "current": progress.get("current"),
            "episodes": episodes,
        })),
    )
        .into_response()
}

const FAVICON_PNG: &[u8] = include_bytes!("../../../../web/favicon.png");

async fn favicon() -> Response {
    // Allow an operator override at DB/config/global/favicon.<ext>; otherwise serve
    // the bundled default.
    let exts = [
        ("png", "image/png"),
        ("ico", "image/x-icon"),
        ("svg", "image/svg+xml"),
        ("jpg", "image/jpeg"),
        ("jpeg", "image/jpeg"),
        ("webp", "image/webp"),
        ("gif", "image/gif"),
    ];
    for (ext, mime) in exts {
        let path = format!("DB/config/global/favicon.{}", ext);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return ([(header::CONTENT_TYPE, mime)], bytes).into_response();
        }
    }
    ([(header::CONTENT_TYPE, "image/png")], FAVICON_PNG).into_response()
}

async fn health() -> &'static str {
    "ok"
}

async fn auth(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string());

    let token = match presented {
        Some(t) if !t.is_empty() => t,
        _ => return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response(),
    };

    let Some(auth) = api_auth_for_token(&token) else {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    };

    if should_rate_limit(req.method(), req.uri().path()) {
        if let Err(retry_after) = st.rate_limiter.check(&auth.token_hash).await {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, retry_after.to_string())],
                "rate limit exceeded",
            )
                .into_response();
        }
    }

    req.extensions_mut().insert(auth);
    next.run(req).await
}

fn should_rate_limit(method: &Method, path: &str) -> bool {
    if method == Method::GET || method == Method::HEAD {
        return false;
    }
    // A node renews every lease every ten seconds for as long as it is working. That is a
    // heartbeat on a fixed cadence, not user traffic, and throttling it would sever the cluster
    // rather than protect anything — the token is already restricted to these routes alone.
    if is_link_path(path) {
        return false;
    }
    !(method == Method::POST && is_cancel_path(path))
}

fn is_link_path(path: &str) -> bool {
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    path.trim_start_matches('/').starts_with("link/")
}

fn is_cancel_path(path: &str) -> bool {
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    parts.len() == 3
        && parts[0] == "jobs"
        && parts[1].parse::<u64>().is_ok()
        && parts[2] == "cancel"
}

struct TokenEntry {
    local_server_id: Option<u64>,
    link_node: Option<String>,
    link_purpose: NodePurpose,
    label: Option<String>,
}

fn parse_token_file(contents: &str) -> std::collections::HashMap<String, TokenEntry> {
    let mut map = std::collections::HashMap::new();
    let mut pending_label: Option<String> = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with(';') {
            pending_label = parse_token_label(line);
            continue;
        }
        let mut parts = line.split('|');
        let stored = parts.next().unwrap_or("").trim();
        let label = pending_label.take();
        if stored.is_empty() {
            continue;
        }
        let (local_server_id, link_node, link_purpose) = match (parts.next(), parts.next()) {
            (Some("local"), Some(server_id)) => {
                (server_id.trim().parse::<u64>().ok(), None, NodePurpose::default())
            }
            (Some("link"), Some(node)) => {
                let node = node.trim();
                // A fourth field names what the node is for. Absent means CPU, which is what every
                // token minted before this field existed means too — those are the CPU boxes the
                // cluster was built out of, and reading them as "anything" would send the first
                // GPU preset to one of them.
                let purpose = parts
                    .next()
                    .map(NodePurpose::parse)
                    .unwrap_or_default();
                (None, (!node.is_empty()).then(|| node.to_string()), purpose)
            }
            _ => (None, None, NodePurpose::default()),
        };
        map.entry(stored.to_string())
            .or_insert(TokenEntry { local_server_id, link_node, link_purpose, label });
    }
    map
}

fn api_auth_for_token(token: &str) -> Option<ApiAuth> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<(Option<std::time::SystemTime>, std::collections::HashMap<String, TokenEntry>)>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new((None, std::collections::HashMap::new())));

    let mtime = std::fs::metadata(API_TOKENS_PATH)
        .and_then(|m| m.modified())
        .ok();

    let mut guard = cache.lock().unwrap();
    if guard.0 != mtime {
        let entries = std::fs::read_to_string(API_TOKENS_PATH)
            .map(|c| parse_token_file(&c))
            .unwrap_or_default();
        *guard = (mtime, entries);
    }

    let entry = guard.1.get(token)?;
    Some(ApiAuth {
        local_server_id: entry.local_server_id,
        link_node: entry.link_node.clone(),
        link_purpose: entry.link_purpose,
        token: token.to_string(),
        token_hash: format!("{:x}", md5::compute(token.as_bytes())),
        label: entry.label.clone(),
    })
}

fn parse_token_label(line: &str) -> Option<String> {
    let body = line.trim_start_matches(';').trim();
    if body.is_empty() {
        return None;
    }
    let label = body.split_once(" (added ")
        .map(|(l, _)| l)
        .unwrap_or(body)
        .trim();
    if label.is_empty() {
        None
    } else {
        Some(label.to_string())
    }
}

pub(super) fn require_pnwitch(auth: &ApiAuth) -> Result<(), Response> {
    if auth.label.as_deref() == Some("PNwitch") {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "PNwitch token required").into_response())
    }
}

// The link routes are the only ones a node token opens, and they are bound to the node named on
// the token line: a node cannot renew or finish a lease held by another. That binding is what makes
// the roster meaningful, since nothing else about a node is authenticated.
// What the presenting token says its node is for. Only `register` asks — the purpose decides
// scheduling, and scheduling happens on the coordinator's side of the roster, never per request.
pub(super) fn link_purpose(auth: &ApiAuth) -> NodePurpose {
    auth.link_purpose
}

pub(super) fn require_link(auth: &ApiAuth) -> Result<String, Response> {
    match auth.link_node.clone() {
        Some(node) => Ok(node),
        None => Err((
            StatusCode::FORBIDDEN,
            "this endpoint requires a link token (mint one with `/gentoken link`)",
        )
            .into_response()),
    }
}

fn effective_server_id(auth: &ApiAuth, requested: Option<u64>) -> Option<u64> {
    auth.local_server_id.or(requested)
}

#[derive(Deserialize)]
struct JobsQuery {
    #[serde(default)]
    status: Option<String>,
}

async fn list_jobs(State(st): State<AppState>, Query(q): Query<JobsQuery>) -> Response {
    let result = match q.status.as_deref() {
        Some("ongoing") => st.db.get_ongoing_jobs().await,
        Some("recent") => st.db.get_recent_jobs(50).await,
        _ => st.db.get_active_jobs().await,
    };
    match result {
        Ok(rows) => Json(rows.iter().map(JobStatus::from_row).collect::<Vec<_>>()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_job(State(st): State<AppState>, Path(id): Path<u64>) -> Response {
    match st.db.get_job(id).await {
        Ok(Some(row)) => Json(JobStatus::from_row(&row)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such job").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// A payload may name the preset its job encodes with, overriding the server default that
// `Job::new_api` has already resolved. The intro group that default carries belongs to the server
// rather than to the preset, so the override keeps it.
fn apply_preset(job: &mut Job, requested: Option<&str>) -> Result<(), Response> {
    let Some(name) = requested.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(());
    };
    let candidates = match &job.preset {
        Preset::PseudoLossless(candidates)
        | Preset::Dummy(candidates)
        | Preset::Standard(candidates)
        | Preset::VerySlow(candidates)
        | Preset::Hd720(candidates)
        | Preset::Sd480(candidates)
        | Preset::Gpu(candidates) => candidates.clone(),
        Preset::Copy => None,
    };
    match preset_from_name(name, candidates) {
        Some(preset) => {
            job.preset = preset;
            Ok(())
        }
        None => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "preset `{name}` is not standard, veryslow, gpu, pseudolossless, dummy, 720p, or 480p"
            ),
        )
            .into_response()),
    }
}

#[derive(Deserialize)]
struct EncodeReq {
    torrent: String,
    subtitle_b64: String,
    #[serde(default)]
    preset: Option<String>,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    channel_id: Option<u64>,
    #[serde(default)]
    server_id: Option<u64>,
    #[serde(default)]
    keep: bool,
    #[serde(default)]
    keyword: Option<String>,
}

async fn submit_encode(State(st): State<AppState>, Extension(auth): Extension<ApiAuth>, Json(req): Json<EncodeReq>) -> Response {
    let subtitle = match base64_decode_bytes(&req.subtitle_b64) {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("subtitle_b64: {e}")).into_response(),
    };
    let mut job = Job::new_api(
        st.api_author,
        req.channel_id.unwrap_or(0),
        JobType::Encode,
        nyaaise(&req.torrent),
        subtitle,
        req.lang.unwrap_or_else(|| "EN".to_string()),
        effective_server_id(&auth, req.server_id),
    );
    if let Err(response) = apply_preset(&mut job, req.preset.as_deref()) {
        return response;
    }
    if req.keep {
        job.keep = Some(KeepRequest::new(req.keyword));
    } else if req.keyword.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false) {
        return (StatusCode::BAD_REQUEST, "keyword requires keep=true").into_response();
    }
    submit(&st, job).await
}

#[derive(Deserialize)]
struct BackupReq {
    torrent: String,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    channel_id: Option<u64>,
    #[serde(default)]
    server_id: Option<u64>,
    #[serde(default)]
    all: bool,
    #[serde(default)]
    keep: bool,
    #[serde(default)]
    keyword: Option<String>,
}

async fn submit_backup(State(st): State<AppState>, Extension(auth): Extension<ApiAuth>, Json(req): Json<BackupReq>) -> Response {
    if req.keep && req.all {
        return (StatusCode::BAD_REQUEST, "keep is not supported with backup all").into_response();
    }
    if !req.keep && req.keyword.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false) {
        return (StatusCode::BAD_REQUEST, "keyword requires keep=true").into_response();
    }
    let job_type = if req.all { JobType::BackupAll } else { JobType::Backup };
    let mut job = Job::new_api(
        st.api_author,
        req.channel_id.unwrap_or(0),
        job_type,
        nyaaise(&req.torrent),
        vec![],
        req.lang.unwrap_or_else(|| "EN".to_string()),
        effective_server_id(&auth, req.server_id),
    );
    if req.keep {
        job.keep = Some(KeepRequest::new(req.keyword));
    }
    submit(&st, job).await
}

#[derive(Deserialize)]
struct ProbeReq {
    torrent: String,
}

async fn submit_probe(State(st): State<AppState>, Json(req): Json<ProbeReq>) -> Response {
    let job = Job::new_api(
        st.api_author,
        0,
        JobType::Probe,
        nyaaise(&req.torrent),
        vec![],
        "EN".to_string(),
        None,
    );
    submit(&st, job).await
}

#[derive(Deserialize)]
struct PancodeReq {
    probe_job_id: String,
    file_index: u64,
    subtitle_b64: String,
    #[serde(default)]
    keep: bool,
    #[serde(default)]
    keyword: Option<String>,
}

async fn submit_pancode(State(st): State<AppState>, Extension(auth): Extension<ApiAuth>, Json(req): Json<PancodeReq>) -> Response {
    let subtitle = match base64_decode_bytes(&req.subtitle_b64) {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("subtitle_b64: {e}")).into_response(),
    };
    let probe_id = match req.probe_job_id.trim().parse::<u64>() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "probe_job_id must be a numeric string").into_response(),
    };
    let probe = match st.db.get_job(probe_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return (StatusCode::NOT_FOUND, "no such probe job").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if probe.archived != 0 {
        return (StatusCode::CONFLICT, "probe job is no longer active").into_response();
    }
    if probe.stage != 21 {
        return (StatusCode::CONFLICT, "probe job is not ready yet").into_response();
    }
    let mut job = Job::new_api(
        st.api_author,
        0,
        JobType::Pancode,
        nyaaise(&probe.link),
        subtitle,
        "EN".to_string(),
        effective_server_id(&auth, None),
    );
    job.probe_job_id = Some(probe_id);
    job.probe_file_index = Some(req.file_index);
    job.display_link = Some(format!("{} : {}", probe.link, req.file_index));
    if req.keep {
        job.keep = Some(KeepRequest::new(req.keyword));
    } else if req.keyword.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false) {
        return (StatusCode::BAD_REQUEST, "keyword requires keep=true").into_response();
    }
    let progress = json!({
        "type": "pancode",
        "torrent": probe.link,
        "probe_job_id": probe_id.to_string(),
        "file_index": req.file_index,
    });
    submit_with_progress(&st, job, Some(progress)).await
}

#[derive(Deserialize)]
struct GitcodeReq {
    torrent: String,
    subtitle_url: String,
    #[serde(default)]
    preset: Option<String>,
    #[serde(default)]
    keep: bool,
    #[serde(default)]
    keyword: Option<String>,
}

async fn submit_gitcode(State(st): State<AppState>, Extension(auth): Extension<ApiAuth>, Json(req): Json<GitcodeReq>) -> Response {
    let url = github_blob_to_raw(&req.subtitle_url);
    let subtitle = match fetch_subtitle(&url).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("subtitle_url: {e}")).into_response(),
    };
    let mut job = Job::new_api(
        st.api_author,
        0,
        JobType::Encode,
        nyaaise(&req.torrent),
        subtitle,
        "EN".to_string(),
        effective_server_id(&auth, None),
    );
    if let Err(response) = apply_preset(&mut job, req.preset.as_deref()) {
        return response;
    }
    if req.keep {
        job.keep = Some(KeepRequest::new(req.keyword));
    } else if req.keyword.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false) {
        return (StatusCode::BAD_REQUEST, "keyword requires keep=true").into_response();
    }
    submit(&st, job).await
}

#[derive(Deserialize)]
struct KeycodeReq {
    keywords: Vec<String>,
    #[serde(default)]
    subtitle_b64: Option<String>,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    channel_id: Option<u64>,
    #[serde(default)]
    server_id: Option<u64>,
}

async fn submit_keycode(State(st): State<AppState>, Extension(auth): Extension<ApiAuth>, Json(req): Json<KeycodeReq>) -> Response {
    let keywords = req
        .keywords
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if keywords.is_empty() {
        return (StatusCode::BAD_REQUEST, "keywords must not be empty").into_response();
    }
    let subtitle = match req.subtitle_b64.as_deref() {
        Some(s) if !s.trim().is_empty() => match base64_decode_bytes(s) {
            Ok(b) => b,
            Err(e) => return (StatusCode::BAD_REQUEST, format!("subtitle_b64: {e}")).into_response(),
        },
        _ => Vec::new(),
    };
    let mut job = Job::new_api(
        st.api_author,
        req.channel_id.unwrap_or(0),
        JobType::Keycode,
        TorrentType::Link("keycode".to_string()),
        subtitle,
        req.lang.unwrap_or_else(|| "EN".to_string()),
        effective_server_id(&auth, req.server_id),
    );
    job.keycode = Some(KeycodeRequest { keywords });
    submit(&st, job).await
}

fn github_blob_to_raw(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("https://github.com/") {
        if let Some((repo, path)) = rest.split_once("/blob/") {
            return format!("https://raw.githubusercontent.com/{}/{}", repo, path);
        }
    }
    url.to_string()
}

async fn fetch_subtitle(url: &str) -> Result<Vec<u8>, String> {
    let url = crate::lib::http::net::sanitize_fetch_url(url).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    Ok(bytes.to_vec())
}

async fn cancel_job(State(st): State<AppState>, Extension(auth): Extension<ApiAuth>, Path(id): Path<u64>) -> Response {
    let row = match st.db.get_job(id).await {
        Ok(Some(row)) => row,
        Ok(None) => return (StatusCode::NOT_FOUND, "no such job").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if let Some(server_id) = auth.local_server_id {
        if row.server_id != Some(server_id as i64) {
            return (StatusCode::FORBIDDEN, "cannot cancel a job from another server").into_response();
        }
        let cancellable = [JobType::Encode, JobType::Studio, JobType::StudioPreview]
            .iter()
            .any(|job_type| row.job_type == *job_type as u16 as i64);
        if !cancellable {
            return (StatusCode::FORBIDDEN, "only encode and Studio jobs can be cancelled through this token").into_response();
        }
    }
    if row.archived != 0 || matches!(row.stage, 6 | 7 | 8 | 9) {
        return (StatusCode::CONFLICT, "job is already terminal").into_response();
    }
    let hj = HalfJob::new_cancel(row.author as u64, row.channel_id as u64, id);
    if st.tx.send(JobClass::HalfJob(hj)).await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "worker channel closed").into_response();
    }
    StatusCode::ACCEPTED.into_response()
}

async fn gitsync(State(st): State<AppState>, Extension(auth): Extension<ApiAuth>) -> Response {
    if let Err(resp) = require_pnwitch(&auth) {
        return resp;
    }
    let hj = HalfJob::new_gitsync_api(st.api_author, 0);
    let job_id = hj.job_id.to_string();
    if st.tx.send(JobClass::HalfJob(hj)).await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "worker channel closed").into_response();
    }
    (StatusCode::ACCEPTED, Json(json!({ "job_id": job_id, "status": "accepted" }))).into_response()
}

pub(super) fn require_local(auth: &ApiAuth) -> Result<u64, Response> {
    match auth.local_server_id {
        Some(id) => Ok(id),
        None => Err((
            StatusCode::FORBIDDEN,
            "this endpoint requires a local token (mint one with `/gentoken local`)",
        ).into_response()),
    }
}

fn parse_channel_id(raw: &str) -> Result<u64, Response> {
    raw.trim().parse::<u64>()
        .map_err(|_| (StatusCode::BAD_REQUEST, "channel_id must be a numeric string").into_response())
}

fn validate_season(season: Option<u64>) -> Result<u16, Response> {
    let s = season.unwrap_or(1);
    if s < 1 || s > u16::MAX as u64 {
        return Err((StatusCode::BAD_REQUEST, "season must be between 1 and 65535").into_response());
    }
    Ok(s as u16)
}

fn credit_or_default(v: Option<String>) -> String {
    v.map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "---".to_string())
}

fn credits_from(tl: Option<String>, tlc: Option<String>, ts: Option<String>, qc: Option<String>) -> Credits {
    Credits {
        tl: credit_or_default(tl),
        tlc: credit_or_default(tlc),
        ts: credit_or_default(ts),
        qc: credit_or_default(qc),
    }
}

fn repo_outcome_response(out: RepoOutcome) -> Response {
    (StatusCode::OK, Json(json!({
        "label": out.label,
        "owner_repo": out.owner_repo,
        "repo_url": out.repo_url,
        "name": out.name,
        "slug": out.slug,
        "kind": out.kind,
        "episode_count": out.episode_count,
        "season": out.season,
        "created": out.created,
        "renamed_files": out.renamed_files,
    }))).into_response()
}

async fn git_attachments(Extension(auth): Extension<ApiAuth>) -> Response {
    let server_id = match require_local(&auth) { Ok(id) => id, Err(r) => return r };
    Json(list_attachments(server_id).await).into_response()
}

async fn git_channels(Extension(auth): Extension<ApiAuth>) -> Response {
    let server_id = match require_local(&auth) { Ok(id) => id, Err(r) => return r };
    let path = format!("DB/config/{}/channels.json", server_id);
    match tokio::fs::read_to_string(&path).await {
        Ok(s) => match serde_json::from_str::<serde_json::Value>(&s) {
            Ok(v) => Json(v).into_response(),
            Err(_) => Json(json!([])).into_response(),
        },
        Err(_) => Json(json!([])).into_response(),
    }
}

async fn git_readmebase(Extension(auth): Extension<ApiAuth>) -> Response {
    let server_id = match require_local(&auth) { Ok(id) => id, Err(r) => return r };
    let server_path = format!("DB/config/{}/base.md", server_id);
    if let Ok(content) = tokio::fs::read_to_string(&server_path).await {
        return Json(json!({ "content": content, "is_guide": false })).into_response();
    }
    let content = tokio::fs::read_to_string("DB/config/global/base.md").await
        .unwrap_or_else(|_| crate::lib::git::README_BASE_GUIDE.to_string());
    Json(json!({ "content": content, "is_guide": true })).into_response()
}

#[derive(Deserialize)]
struct GitReadmebaseReq {
    content: String,
}

async fn git_readmebase_set(Extension(auth): Extension<ApiAuth>, Json(req): Json<GitReadmebaseReq>) -> Response {
    let server_id = match require_local(&auth) { Ok(id) => id, Err(r) => return r };
    let dir = format!("DB/config/{}", server_id);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to create config dir: {}", e)).into_response();
    }
    let path = format!("{}/base.md", dir);
    match tokio::fs::write(&path, req.content.as_bytes()).await {
        Ok(()) => Json(json!({ "saved": true, "bytes": req.content.len() })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to write base.md: {}", e)).into_response(),
    }
}

#[derive(Deserialize)]
struct GitInitReq {
    mal: String,
    channel_id: String,
    #[serde(default)]
    season: Option<u64>,
    #[serde(default)]
    tl: Option<String>,
    #[serde(default)]
    tlc: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    qc: Option<String>,
}

async fn git_init(Extension(auth): Extension<ApiAuth>, Json(req): Json<GitInitReq>) -> Response {
    let server_id = match require_local(&auth) { Ok(id) => id, Err(r) => return r };
    let channel_id = match parse_channel_id(&req.channel_id) { Ok(c) => c, Err(r) => return r };
    let season = match validate_season(req.season) { Ok(s) => s, Err(r) => return r };
    let credits = credits_from(req.tl, req.tlc, req.ts, req.qc);
    match init_repo(server_id, channel_id, &req.mal, season, &credits).await {
        Ok(out) => repo_outcome_response(out),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

#[derive(Deserialize)]
struct GitAttachReq {
    mal: String,
    repo: String,
    channel_id: String,
    #[serde(default)]
    season: Option<u64>,
    #[serde(default)]
    tl: Option<String>,
    #[serde(default)]
    tlc: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    qc: Option<String>,
}

async fn git_attach(Extension(auth): Extension<ApiAuth>, Json(req): Json<GitAttachReq>) -> Response {
    let server_id = match require_local(&auth) { Ok(id) => id, Err(r) => return r };
    let channel_id = match parse_channel_id(&req.channel_id) { Ok(c) => c, Err(r) => return r };
    let season = match validate_season(req.season) { Ok(s) => s, Err(r) => return r };
    let credits = credits_from(req.tl, req.tlc, req.ts, req.qc);
    match attach_repo(server_id, channel_id, &req.mal, &req.repo, season, &credits).await {
        Ok(out) => repo_outcome_response(out),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

#[derive(Deserialize)]
struct GitSourceReq {
    channel_id: String,
    episode: u32,
    link: String,
}

async fn git_source(Extension(auth): Extension<ApiAuth>, Json(req): Json<GitSourceReq>) -> Response {
    let server_id = match require_local(&auth) { Ok(id) => id, Err(r) => return r };
    let channel_id = match parse_channel_id(&req.channel_id) { Ok(c) => c, Err(r) => return r };
    match set_source(server_id, channel_id, req.episode, &req.link).await {
        Ok(out) => (StatusCode::OK, Json(json!({
            "path": out.path,
            "content": out.content,
        }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

#[derive(Deserialize)]
struct GitChannelReq {
    channel_id: String,
}

async fn git_detach(Extension(auth): Extension<ApiAuth>, Json(req): Json<GitChannelReq>) -> Response {
    let server_id = match require_local(&auth) { Ok(id) => id, Err(r) => return r };
    let channel_id = match parse_channel_id(&req.channel_id) { Ok(c) => c, Err(r) => return r };
    match detach_channel(server_id, channel_id).await {
        Ok(out) => (StatusCode::OK, Json(json!({
            "name": out.name,
            "repo_url": out.repo_url,
        }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

async fn git_destruct(Extension(auth): Extension<ApiAuth>, Json(req): Json<GitChannelReq>) -> Response {
    let server_id = match require_local(&auth) { Ok(id) => id, Err(r) => return r };
    let channel_id = match parse_channel_id(&req.channel_id) { Ok(c) => c, Err(r) => return r };
    match destruct_repo(server_id, channel_id).await {
        Ok(out) => (StatusCode::OK, Json(json!({
            "name": out.name,
            "owner_repo": out.owner_repo,
        }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

#[derive(Deserialize)]
struct GitSmartcodeReq {
    channel_id: String,
    episode: u32,
    #[serde(default)]
    link: Option<String>,
    #[serde(default)]
    keep: bool,
    #[serde(default)]
    keyword: Option<String>,
}

async fn git_smartcode(State(st): State<AppState>, Extension(auth): Extension<ApiAuth>, Json(req): Json<GitSmartcodeReq>) -> Response {
    let server_id = match require_local(&auth) { Ok(id) => id, Err(r) => return r };
    let channel_id = match parse_channel_id(&req.channel_id) { Ok(c) => c, Err(r) => return r };
    let link_opt = req.link.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let merge = match smartcode_merge(server_id, channel_id, req.episode, link_opt).await {
        Ok(m) => m,
        Err(e) => return (StatusCode::BAD_GATEWAY, e).into_response(),
    };
    let mut job = Job::new_api(
        st.api_author,
        channel_id,
        JobType::Encode,
        nyaaise(&merge.link),
        merge.merged_bytes,
        "EN".to_string(),
        Some(server_id),
    );
    job.smartcode_drive_name = Some(SmartcodeDriveName::new(
        &merge.owner_repo,
        &merge.gdrive_folder_local,
        req.episode,
    ));
    job.gdrive_folder_global = Some(merge.gdrive_folder_global.clone());
    job.gdrive_folder_local = Some(merge.gdrive_folder_local.clone());
    if req.keep {
        job.keep = Some(KeepRequest::new(req.keyword));
    } else if req.keyword.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false) {
        return (StatusCode::BAD_REQUEST, "keyword requires keep=true").into_response();
    }
    let job_id = job.job_id;
    if let Err(e) = st.db.insert_job(&job).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    if st.tx.send(JobClass::Job(job)).await.is_err() {
        let _ = st.db.update_stage(job_id, Stage::Failed).await;
        let _ = st.db.archive_job(job_id).await;
        return (StatusCode::SERVICE_UNAVAILABLE, "worker channel closed").into_response();
    }
    (StatusCode::ACCEPTED, Json(json!({
        "job_id": job_id.to_string(),
        "link": merge.link,
        "owner_repo": merge.owner_repo,
        "release_path": merge.release_path,
        "source_path": merge.source_path,
        "gdrive_folder_global": merge.gdrive_folder_global,
        "gdrive_folder_local": merge.gdrive_folder_local,
        "warnings": merge.warnings,
    }))).into_response()
}

pub(super) async fn submit(st: &AppState, job: Job) -> Response {
    submit_with_progress(st, job, None).await
}

async fn submit_with_progress(st: &AppState, job: Job, progress: Option<serde_json::Value>) -> Response {
    let job_id = job.job_id;
    let directory = job.directory.clone();
    if let Err(e) = st.db.insert_job(&job).await {
        tokio::fs::remove_dir_all(&directory).await.ok();
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    if let Some(v) = progress {
        if let Err(e) = st.db.update_progress(job_id, &v.to_string()).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }
    if st.tx.send(JobClass::Job(job)).await.is_err() {
        let _ = st.db.update_stage(job_id, Stage::Failed).await;
        let _ = st.db.archive_job(job_id).await;
        tokio::fs::remove_dir_all(directory).await.ok();
        return (StatusCode::SERVICE_UNAVAILABLE, "worker channel closed").into_response();
    }
    (StatusCode::ACCEPTED, Json(json!({ "job_id": job_id.to_string() }))).into_response()
}

async fn acix_confirm(State(st): State<AppState>, Path(id): Path<u64>) -> Response {
    match confirm_acix(&st.db, id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

fn acix_default_limit() -> u32 { 20 }

#[derive(Deserialize)]
struct AcixSearchQuery {
    q: String,
    #[serde(default = "acix_default_limit")]
    limit: u32,
}

async fn acix_search(Json(q): Json<AcixSearchQuery>) -> Response {
    let client = match AnimeCix::from_token_env() {
        Ok(c) => c,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e).into_response(),
    };
    match client.search(&q.q, q.limit).await {
        Ok(hits) => Json(hits).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

#[derive(Deserialize)]
struct AcixTmdbReq {
    tmdb_id: String,
    #[serde(default)]
    media_type: Option<String>,
}

async fn acix_tmdb(Json(req): Json<AcixTmdbReq>) -> Response {
    let mt = match req.media_type.as_deref() {
        Some("movies") | Some("movie") => MediaType::Movies,
        _ => MediaType::Series,
    };
    let client = match AnimeCix::from_env() {
        Ok(c) => c,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e).into_response(),
    };
    match client.tmdb_to_acix(&req.tmdb_id, mt).await {
        Ok(r) => Json(json!({ "status": r.status, "acix_id": r.acix_id, "body": r.body })).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

async fn acix_translators() -> Response {
    let client = match AnimeCix::from_token_env() {
        Ok(c) => c,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e).into_response(),
    };
    match client.translators().await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

#[derive(Deserialize)]
struct AcixPublishReq {
    extra: String,
    url: String,
    template: i64,
    title_id: i64,
    #[serde(default)]
    season_num: Option<i64>,
    #[serde(default)]
    episode_num: Option<i64>,
}

async fn acix_publish(Extension(auth): Extension<ApiAuth>, Json(req): Json<AcixPublishReq>) -> Response {
    if let Err(resp) = require_pnwitch(&auth) {
        return resp;
    }
    let client = match AnimeCix::from_env() {
        Ok(c) => c,
        Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, e).into_response(),
    };
    let up = MixedUpload::new(req.extra, req.url, req.template, req.title_id, req.season_num, req.episode_num);
    match client.multishare_mixed(&up).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

pub(super) fn base64_decode_bytes(input: &str) -> Result<Vec<u8>, String> {
    const ALPH: [u8; 128] = {
        let mut a = [255u8; 128];
        let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0;
        while i < chars.len() {
            a[chars[i] as usize] = i as u8;
            i += 1;
        }
        a
    };
    let cleaned: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if cleaned.len() % 4 != 0 {
        return Err(format!("invalid length {}", cleaned.len()));
    }
    let mut out: Vec<u8> = Vec::with_capacity(cleaned.len() / 4 * 3);
    let mut i = 0;
    while i < cleaned.len() {
        let c0 = cleaned[i];
        let c1 = cleaned[i + 1];
        let c2 = cleaned[i + 2];
        let c3 = cleaned[i + 3];
        let pad2 = c2 == b'=';
        let pad3 = c3 == b'=';
        let v0 = ALPH[c0 as usize % 128];
        let v1 = ALPH[c1 as usize % 128];
        if c0 > 127 || c1 > 127 || v0 == 255 || v1 == 255 {
            return Err(format!("invalid char at {}", i));
        }
        out.push((v0 << 2) | (v1 >> 4));
        if !pad2 {
            if c2 > 127 || ALPH[c2 as usize] == 255 {
                return Err(format!("invalid char at {}", i + 2));
            }
            let v2 = ALPH[c2 as usize];
            out.push((v1 << 4) | (v2 >> 2));
            if !pad3 {
                if c3 > 127 || ALPH[c3 as usize] == 255 {
                    return Err(format!("invalid char at {}", i + 3));
                }
                let v3 = ALPH[c3 as usize];
                out.push((v2 << 6) | v3);
            }
        }
        i += 4;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Node tokens share the token file with every other kind, so the parser has to keep the three
    // line shapes apart: a plain token, a server-bound one, and a node-bound one.
    #[test]
    fn the_token_file_keeps_link_local_and_plain_tokens_apart() {
        let entries = parse_token_file(
            "; PNwitch (added 1)\n\
             aaaa\n\
             ; console (added 2)\n\
             bbbb|local|1234\n\
             ; osaka box (added 3)\n\
             cccc|link|mini-osaka\n",
        );
        let plain = entries.get("aaaa").expect("plain token missing");
        assert_eq!(plain.local_server_id, None);
        assert_eq!(plain.link_node, None);
        assert_eq!(plain.label.as_deref(), Some("PNwitch"));

        let local = entries.get("bbbb").expect("local token missing");
        assert_eq!(local.local_server_id, Some(1234));
        assert_eq!(local.link_node, None);

        let link = entries.get("cccc").expect("link token missing");
        assert_eq!(link.local_server_id, None);
        assert_eq!(link.link_node.as_deref(), Some("mini-osaka"));
        // No fourth field: every token minted before the field existed belongs to a CPU box.
        assert_eq!(link.link_purpose, NodePurpose::Cpu);
    }

    // The fourth field is what keeps a GPU preset off a machine without one. It has to come from
    // the token rather than from the node, so a node cannot widen what it is offered.
    #[test]
    fn a_link_tokens_fourth_field_is_the_nodes_purpose() {
        let entries = parse_token_file(
            "aaaa|link|mini-gpu|gpu
             bbbb|link|mini-cpu|cpu
             cccc|link|mini-any|both
             dddd|link|mini-typo|gpus
",
        );
        assert_eq!(entries["aaaa"].link_purpose, NodePurpose::Gpu);
        assert_eq!(entries["bbbb"].link_purpose, NodePurpose::Cpu);
        assert_eq!(entries["cccc"].link_purpose, NodePurpose::Both);
        // A typo must narrow rather than widen: reading it as "anything" would put GPU work on a
        // box chosen by a misspelling.
        assert_eq!(entries["dddd"].link_purpose, NodePurpose::Cpu);
        // The node name is still the node name whatever follows it.
        assert_eq!(entries["aaaa"].link_node.as_deref(), Some("mini-gpu"));
    }

    // A node token must not be mistaken for a server-bound one: `require_local` gates the git
    // endpoints, and a node has no business there.
    #[test]
    fn a_link_token_is_not_a_local_token() {
        let entries = parse_token_file("dddd|link|mini-a\n");
        assert_eq!(entries["dddd"].local_server_id, None);
    }

    // A node renews every lease every ten seconds for as long as it works. Under the default
    // thirty-writes-per-minute limit that alone would start returning 429 and sever the cluster.
    #[test]
    fn link_traffic_is_not_rate_limited() {
        assert!(!should_rate_limit(&Method::POST, "/api/v1/link/lease/42/renew"));
        assert!(!should_rate_limit(&Method::POST, "/api/v1/link/register"));
        assert!(!should_rate_limit(&Method::POST, "/api/v1/link/lease/42/result"));
        // Everything else still is.
        assert!(should_rate_limit(&Method::POST, "/api/v1/jobs/encode"));
        // Including a path that merely mentions the word.
        assert!(should_rate_limit(&Method::POST, "/api/v1/jobs/linkedy"));
    }
}
