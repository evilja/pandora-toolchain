use std::time::Duration;

use axum::{
    Json,
    extract::{Extension, Path, Query},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::core::{ApiAuth, require_link};
use crate::pnworker::link::board;
use crate::pnworker::link::client::encoder_digest;
use crate::pnworker::link::spec::{LeaseRenew, LeaseResult, NodeRegister};

// The coordinator's half of the link. A node has no inbound surface, so every exchange starts here
// as a request from the node: it registers, long-polls for work, renews while it works, and reports
// the outcome. Nothing in this file touches the queue directly — `board` is the handoff to
// `pn_worker`, which is the only thing allowed to own a job.

// How long a lease request waits before answering "nothing for you". Long enough that an idle
// cluster costs one request every half minute per node, short enough to sit well inside any proxy's
// idle timeout — a Cloudflare tunnel among them.
const LEASE_POLL_SECS: u64 = 30;
const LEASE_POLL_TICK_MS: u64 = 500;

#[derive(Deserialize)]
pub(super) struct NodeQuery {
    node: String,
}

pub(super) async fn register(
    Extension(auth): Extension<ApiAuth>,
    Json(body): Json<NodeRegister>,
) -> Response {
    let node = match require_link(&auth) {
        Ok(node) => node,
        Err(response) => return response,
    };
    if body.node != node {
        return name_mismatch(&node, &body.node);
    }
    let registered = board::register(body, &encoder_digest());
    if !registered.accepted {
        println!(
            "[link] {} | registration refused: {}",
            node,
            registered.reason.clone().unwrap_or_default()
        );
    } else {
        println!("[link] {node} | registered");
    }
    Json(registered).into_response()
}

pub(super) async fn lease(
    Extension(auth): Extension<ApiAuth>,
    Query(query): Query<NodeQuery>,
) -> Response {
    let node = match require_link(&auth) {
        Ok(node) => node,
        Err(response) => return response,
    };
    if query.node != node {
        return name_mismatch(&node, &query.node);
    }
    board::touch(&node);
    // A long poll rather than a notification: the node holds the request open and the coordinator
    // answers the moment `pn_worker` offers it something. It costs one parked request per idle
    // node and saves every node an inbound hostname.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(LEASE_POLL_SECS);
    loop {
        if let Some(spec) = board::claim(&node) {
            println!("[link] {node} | leased job {}", spec.job_id);
            return Json(spec).into_response();
        }
        if tokio::time::Instant::now() >= deadline {
            return StatusCode::NO_CONTENT.into_response();
        }
        tokio::time::sleep(Duration::from_millis(LEASE_POLL_TICK_MS)).await;
    }
}

pub(super) async fn renew(
    Extension(auth): Extension<ApiAuth>,
    Path(job_id): Path<u64>,
    Json(body): Json<LeaseRenew>,
) -> Response {
    let node = match require_link(&auth) {
        Ok(node) => node,
        Err(response) => return response,
    };
    if body.node != node {
        return name_mismatch(&node, &body.node);
    }
    Json(board::renew(job_id, body)).into_response()
}

pub(super) async fn result(
    Extension(auth): Extension<ApiAuth>,
    Path(job_id): Path<u64>,
    Json(body): Json<LeaseResult>,
) -> Response {
    let node = match require_link(&auth) {
        Ok(node) => node,
        Err(response) => return response,
    };
    if body.node != node {
        return name_mismatch(&node, &body.node);
    }
    let outcome = body.outcome;
    if !board::finish(job_id, body) {
        // The lease was already reclaimed, or belongs to another node. Answering `409` rather than
        // an error tells the node to stop retrying without making it look like a transport fault.
        return (StatusCode::CONFLICT, "no such lease for this node").into_response();
    }
    println!("[link] {node} | job {job_id} reported {outcome:?}");
    StatusCode::ACCEPTED.into_response()
}

// The token names the node; the body has to agree. Without this a node holding a valid token could
// renew or finish a lease belonging to a different one.
fn name_mismatch(token_node: &str, claimed: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        format!("this token is bound to node {token_node}, not {claimed}"),
    )
        .into_response()
}
