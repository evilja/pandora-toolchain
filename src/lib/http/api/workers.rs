use axum::{
    Json,
    extract::Extension,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::pnworker::snapshot;

use super::core::{ApiAuth, require_privileged};

// The live view of the worker loop: heartbeats, reboot counts, and every in-flight job's dispatch
// and last-frame ages. None of that reaches the jobs table, so this is the only way to see a stall
// while it is happening rather than reconstructing it from a corpse afterwards.
pub(super) async fn workers(Extension(auth): Extension<ApiAuth>) -> Response {
    if let Err(resp) = require_privileged(&auth) {
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

pub(super) async fn summary() -> Response {
    match snapshot::summary() {
        Some(summary) => ([(header::CACHE_CONTROL, "no-store")], Json(summary)).into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, "worker has not published a snapshot yet").into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct EventsQuery {
    limit: Option<usize>,
}

pub(super) async fn events(axum::extract::Query(query): axum::extract::Query<EventsQuery>) -> Response {
    let events = snapshot::recent_events(query.limit.unwrap_or(50));
    ([(header::CACHE_CONTROL, "no-store")], Json(events)).into_response()
}
