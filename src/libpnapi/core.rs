use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::mpsc::Sender;

use crate::pnworker::core::{HalfJob, Job, JobClass, JobType, Preset, Stage};
use crate::pnworker::acix::confirm_acix;
use crate::libacix::{AnimeCix, MediaType, MixedUpload};
use crate::libpndb::core::{JobDb, JobStatus};
use crate::libpngit::{
    attach_repo, destruct_repo, detach_channel, init_repo, list_attachments, set_source,
    smartcode_merge, Credits, RepoOutcome,
};
use crate::libpnp2p::nyaaise::nyaaise;
use crate::libpnenv::core::{get_pandora_env, get_perm};
use crate::libpnenv::standard::{API_AUTHOR_ID, API_HOST, API_TOKENS_PATH};

#[derive(Clone)]
struct AppState {
    tx: Sender<JobClass>,
    db: Arc<JobDb>,
    api_author: u64,
}

#[derive(Clone)]
struct ApiAuth {
    local_server_id: Option<u64>,
}

pub async fn serve(tx: Sender<JobClass>, port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let env = get_pandora_env();
    let db = Arc::new(JobDb::new().await?);
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
    let state = AppState { tx, db, api_author };

    let protected = Router::new()
        .route("/jobs", get(list_jobs))
        .route("/jobs/:id", get(get_job))
        .route("/jobs/encode", post(submit_encode))
        .route("/jobs/backup", post(submit_backup))
        .route("/jobs/probe", post(submit_probe))
        .route("/jobs/pancode", post(submit_pancode))
        .route("/jobs/gitcode", post(submit_gitcode))
        .route("/jobs/:id/cancel", post(cancel_job))
        .route("/jobs/:id/acix/confirm", post(acix_confirm))
        .route("/git/attachments", get(git_attachments))
        .route("/git/channels", get(git_channels))
        .route("/git/readmebase", get(git_readmebase))
        .route("/git/init", post(git_init))
        .route("/git/attach", post(git_attach))
        .route("/git/source", post(git_source))
        .route("/git/detach", post(git_detach))
        .route("/git/destruct", post(git_destruct))
        .route("/git/smartcode", post(git_smartcode))
        .route("/gitsync", post(gitsync))
        .route("/acix/search", get(acix_search))
        .route("/acix/tmdb", post(acix_tmdb))
        .route("/acix/translators", get(acix_translators))
        .route("/acix/publish", post(acix_publish))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .layer(middleware::from_fn(auth));

    let app = Router::new()
        .route("/", get(index))
        .route("/git", get(git_console))
        .route("/health", get(health))
        .nest("/api/v1", protected)
        .with_state(state);

    let addr = SocketAddr::new(host, port);
    let listener = TcpListener::bind(addr).await?;
    println!("[Pandora API] serving console + API on http://{}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}

const INDEX_HTML: &str = include_str!("../../web/index.html");
const GIT_HTML: &str = include_str!("../../web/git.html");

async fn index() -> axum::response::Html<&'static str> {
    axum::response::Html(INDEX_HTML)
}

async fn git_console() -> axum::response::Html<&'static str> {
    axum::response::Html(GIT_HTML)
}

async fn health() -> &'static str {
    "ok"
}

async fn auth(mut req: Request, next: Next) -> Response {
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

    req.extensions_mut().insert(auth);
    next.run(req).await
}

fn api_auth_for_token(token: &str) -> Option<ApiAuth> {
    for line in get_perm(API_TOKENS_PATH.to_string()) {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        let mut parts = line.split('|');
        let stored = parts.next().unwrap_or("").trim();
        if stored != token {
            continue;
        }
        let local_server_id = match (parts.next(), parts.next()) {
            (Some("local"), Some(server_id)) => server_id.trim().parse::<u64>().ok(),
            _ => None,
        };
        return Some(ApiAuth { local_server_id });
    }
    None
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
}

async fn submit_encode(State(st): State<AppState>, Extension(auth): Extension<ApiAuth>, Json(req): Json<EncodeReq>) -> Response {
    let subtitle = match base64_decode_bytes(&req.subtitle_b64) {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("subtitle_b64: {e}")).into_response(),
    };
    let job = Job::new_api(
        st.api_author,
        req.channel_id.unwrap_or(0),
        JobType::Encode,
        preset_from_str(req.preset.as_deref()),
        nyaaise(&req.torrent),
        subtitle,
        req.lang.unwrap_or_else(|| "EN".to_string()),
        effective_server_id(&auth, req.server_id),
    );
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
}

async fn submit_backup(State(st): State<AppState>, Extension(auth): Extension<ApiAuth>, Json(req): Json<BackupReq>) -> Response {
    let job_type = if req.all { JobType::BackupAll } else { JobType::Backup };
    let job = Job::new_api(
        st.api_author,
        req.channel_id.unwrap_or(0),
        job_type,
        Preset::Dummy(None),
        nyaaise(&req.torrent),
        vec![],
        req.lang.unwrap_or_else(|| "EN".to_string()),
        effective_server_id(&auth, req.server_id),
    );
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
        Preset::Dummy(None),
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
    preset: Option<String>,
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
        preset_from_str(req.preset.as_deref()),
        nyaaise(&probe.link),
        subtitle,
        "EN".to_string(),
        effective_server_id(&auth, None),
    );
    job.probe_job_id = Some(probe_id);
    job.probe_file_index = Some(req.file_index);
    submit(&st, job).await
}

#[derive(Deserialize)]
struct GitcodeReq {
    torrent: String,
    subtitle_url: String,
    #[serde(default)]
    preset: Option<String>,
}

async fn submit_gitcode(State(st): State<AppState>, Extension(auth): Extension<ApiAuth>, Json(req): Json<GitcodeReq>) -> Response {
    let url = github_blob_to_raw(&req.subtitle_url);
    let subtitle = match fetch_subtitle(&url).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("subtitle_url: {e}")).into_response(),
    };
    let job = Job::new_api(
        st.api_author,
        0,
        JobType::Encode,
        preset_from_str(req.preset.as_deref()),
        nyaaise(&req.torrent),
        subtitle,
        "EN".to_string(),
        effective_server_id(&auth, None),
    );
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    Ok(bytes.to_vec())
}

async fn cancel_job(State(st): State<AppState>, Path(id): Path<u64>) -> Response {
    let hj = HalfJob::new_cancel(st.api_author, 0, id);
    if st.tx.send(JobClass::HalfJob(hj)).await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "worker channel closed").into_response();
    }
    StatusCode::ACCEPTED.into_response()
}

async fn gitsync(State(st): State<AppState>) -> Response {
    let hj = HalfJob::new_gitsync_api(st.api_author, 0);
    let job_id = hj.job_id.to_string();
    if st.tx.send(JobClass::HalfJob(hj)).await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "worker channel closed").into_response();
    }
    (StatusCode::ACCEPTED, Json(json!({ "job_id": job_id, "status": "accepted" }))).into_response()
}

fn require_local(auth: &ApiAuth) -> Result<u64, Response> {
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
        .unwrap_or_else(|_| crate::libpngit::README_BASE_GUIDE.to_string());
    Json(json!({ "content": content, "is_guide": true })).into_response()
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
    preset: Option<String>,
}

async fn git_smartcode(State(st): State<AppState>, Extension(auth): Extension<ApiAuth>, Json(req): Json<GitSmartcodeReq>) -> Response {
    let server_id = match require_local(&auth) { Ok(id) => id, Err(r) => return r };
    let channel_id = match parse_channel_id(&req.channel_id) { Ok(c) => c, Err(r) => return r };
    let link_opt = req.link.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let merge = match smartcode_merge(server_id, channel_id, req.episode, link_opt).await {
        Ok(m) => m,
        Err(e) => return (StatusCode::BAD_GATEWAY, e).into_response(),
    };
    let job = Job::new_api(
        st.api_author,
        channel_id,
        JobType::Encode,
        preset_from_str(req.preset.as_deref()),
        nyaaise(&merge.link),
        merge.merged_bytes,
        "EN".to_string(),
        Some(server_id),
    );
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
        "warnings": merge.warnings,
    }))).into_response()
}

async fn submit(st: &AppState, job: Job) -> Response {
    let job_id = job.job_id;
    if let Err(e) = st.db.insert_job(&job).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    if st.tx.send(JobClass::Job(job)).await.is_err() {
        let _ = st.db.update_stage(job_id, Stage::Failed).await;
        let _ = st.db.archive_job(job_id).await;
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

async fn acix_search(Query(q): Query<AcixSearchQuery>) -> Response {
    let client = match AnimeCix::from_env() {
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
    let client = match AnimeCix::from_env() {
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

async fn acix_publish(Json(req): Json<AcixPublishReq>) -> Response {
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

fn preset_from_str(s: Option<&str>) -> Preset {
    match s.unwrap_or("standard") {
        "gpu" | "standard" => Preset::Standard(None),
        "dummy" => Preset::Dummy(None),
        _ => Preset::PseudoLossless(None),
    }
}

fn base64_decode_bytes(input: &str) -> Result<Vec<u8>, String> {
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
