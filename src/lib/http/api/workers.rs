use axum::{
    Json,
    extract::Extension,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::pnworker::snapshot;

use super::core::{ApiAuth, require_pnwitch};

// The live view of the worker loop: heartbeats, reboot counts, and every in-flight job's dispatch
// and last-frame ages. None of that reaches the jobs table, so this is the only way to see a stall
// while it is happening rather than reconstructing it from a corpse afterwards.
pub(super) async fn workers(Extension(auth): Extension<ApiAuth>) -> Response {
    if let Err(resp) = require_pnwitch(&auth) {
        return resp;
    }
    match snapshot::current() {
        Some(snapshot) => (
            [(header::CACHE_CONTROL, "no-store")],
            Json(snapshot),
        )
            .into_response(),
        // The API can be up while the worker loop is not: `serve` is its own task.
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "worker has not published a snapshot yet",
        )
            .into_response(),
    }
}
