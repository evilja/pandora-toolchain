use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;

use crate::lib::db::core::JobStatus;
use crate::lib::joblog::{JobLogs, find_job_logs, read_job_log, zip_log_files};

use super::core::{ApiAuth, AppState, require_pnwitch};

// Default slice served for a single log file; encoder logs run to hundreds of
// MB and a debugging read only ever wants the end of one.
const DEFAULT_LOG_BYTES: u64 = 1024 * 1024;
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Deserialize)]
pub(super) struct LogReadQuery {
    #[serde(default)]
    tail: Option<usize>,
    #[serde(default)]
    max_bytes: Option<u64>,
}

// The API counterpart of `/catlogs`: the same DB/work → DB/saved_data lookup,
// but listing and reading files individually so a caller can inspect a stuck
// job without downloading the whole archive.
pub(super) async fn list_logs(
    State(st): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Path(id): Path<u64>,
) -> Response {
    if let Err(resp) = require_pnwitch(&auth) {
        return resp;
    }
    let logs = match lookup(id).await {
        Ok(logs) => logs,
        Err(resp) => return resp,
    };
    // The job row is what tells a reader which stage the logs stopped at, so
    // include it when the job is still in the DB.
    let job = st
        .db
        .get_job(id)
        .await
        .ok()
        .flatten()
        .map(|row| JobStatus::from_row(&row));
    Json(json!({
        "job_id": id.to_string(),
        "location": logs.location.as_str(),
        "total_bytes": logs.total_bytes(),
        "files": logs.files.iter().map(|file| json!({
            "name": file.name,
            "bytes": file.bytes,
            "modified": file.modified,
        })).collect::<Vec<_>>(),
        "job": job,
    }))
    .into_response()
}

pub(super) async fn read_log(
    Extension(auth): Extension<ApiAuth>,
    Path((id, name)): Path<(u64, String)>,
    Query(q): Query<LogReadQuery>,
) -> Response {
    if let Err(resp) = require_pnwitch(&auth) {
        return resp;
    }
    let logs = match lookup(id).await {
        Ok(logs) => logs,
        Err(resp) => return resp,
    };
    // Matching against the listed names is also the path-traversal guard: a
    // `..` or absolute name simply never matches an entry.
    let Some(file) = logs.file(&name) else {
        return (StatusCode::NOT_FOUND, "no such log file for this job").into_response();
    };
    let max_bytes = q.max_bytes.unwrap_or(DEFAULT_LOG_BYTES).clamp(1, MAX_LOG_BYTES);
    match read_job_log(file, max_bytes, q.tail).await {
        Ok(log) => (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8".to_string()),
                (header::CACHE_CONTROL, "no-store".to_string()),
                (
                    header::HeaderName::from_static("x-pandora-log-bytes"),
                    log.bytes.to_string(),
                ),
                (
                    header::HeaderName::from_static("x-pandora-log-truncated"),
                    log.truncated.to_string(),
                ),
            ],
            log.text,
        )
            .into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    }
}

pub(super) async fn download_logs(
    Extension(auth): Extension<ApiAuth>,
    Path(id): Path<u64>,
) -> Response {
    if let Err(resp) = require_pnwitch(&auth) {
        return resp;
    }
    let logs = match lookup(id).await {
        Ok(logs) => logs,
        Err(resp) => return resp,
    };
    match zip_log_files(&logs.files).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/zip".to_string()),
                (header::CACHE_CONTROL, "no-store".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"pandora-logs-{}.zip\"", id),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    }
}

async fn lookup(job_id: u64) -> Result<JobLogs, Response> {
    match find_job_logs(job_id).await {
        Ok(Some(logs)) => Ok(logs),
        Ok(None) => Err((StatusCode::NOT_FOUND, "no logs for this job").into_response()),
        Err(error) => Err((StatusCode::INTERNAL_SERVER_ERROR, error).into_response()),
    }
}
