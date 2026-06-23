use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::mpsc::Sender;

use crate::pnworker::core::{HalfJob, Job, JobClass, JobType, Preset};
use crate::libpndb::core::{JobDb, JobStatus};
use crate::libpnp2p::nyaaise::nyaaise;
use crate::libpnenv::core::{get_pandora_env, get_perm};
use crate::libpnenv::standard::{API_AUTHOR_ID, API_HOST, API_TOKENS_PATH};

#[derive(Clone)]
struct AppState {
    tx: Sender<JobClass>,
    db: Arc<JobDb>,
    api_author: u64,
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
        .route("/jobs/:id/cancel", post(cancel_job))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .layer(middleware::from_fn(auth));

    let app = Router::new()
        .route("/", get(index))
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

async fn index() -> axum::response::Html<&'static str> {
    axum::response::Html(INDEX_HTML)
}

async fn health() -> &'static str {
    "ok"
}

async fn auth(req: Request, next: Next) -> Response {
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

    let valid = get_perm(API_TOKENS_PATH.to_string())
        .into_iter()
        .map(|l| l.trim().to_string())
        .any(|t| !t.is_empty() && !t.starts_with(';') && t == token);

    if !valid {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }

    next.run(req).await
}

#[derive(Deserialize)]
struct JobsQuery {
    #[serde(default)]
    status: Option<String>,
}

async fn list_jobs(State(st): State<AppState>, Query(q): Query<JobsQuery>) -> Response {
    let result = match q.status.as_deref() {
        Some("ongoing") => st.db.get_ongoing_jobs().await,
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

async fn submit_encode(State(st): State<AppState>, Json(req): Json<EncodeReq>) -> Response {
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
        req.server_id,
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

async fn submit_backup(State(st): State<AppState>, Json(req): Json<BackupReq>) -> Response {
    let job_type = if req.all { JobType::BackupAll } else { JobType::Backup };
    let job = Job::new_api(
        st.api_author,
        req.channel_id.unwrap_or(0),
        job_type,
        Preset::Dummy(None),
        nyaaise(&req.torrent),
        vec![],
        req.lang.unwrap_or_else(|| "EN".to_string()),
        req.server_id,
    );
    submit(&st, job).await
}

async fn cancel_job(State(st): State<AppState>, Path(id): Path<u64>) -> Response {
    let hj = HalfJob::new_cancel(st.api_author, 0, id);
    if st.tx.send(JobClass::HalfJob(hj)).await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "worker channel closed").into_response();
    }
    StatusCode::ACCEPTED.into_response()
}

async fn submit(st: &AppState, job: Job) -> Response {
    let job_id = job.job_id;
    if st.tx.send(JobClass::Job(job)).await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "worker channel closed").into_response();
    }
    (StatusCode::ACCEPTED, Json(json!({ "job_id": job_id.to_string() }))).into_response()
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
