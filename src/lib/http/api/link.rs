use std::time::Duration;

use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, Query},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_lite::StreamExt;
use tokio::io::AsyncWriteExt;
use serde::Deserialize;

use super::core::{ApiAuth, require_link};
use crate::pnworker::link::assets;
use crate::pnworker::link::board;
use crate::pnworker::link::client::encoder_identity;
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
    let registered = board::register(body, &encoder_identity());
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
    Json(mut body): Json<LeaseRenew>,
) -> Response {
    let node = match require_link(&auth) {
        Ok(node) => node,
        Err(response) => return response,
    };
    if body.node != node {
        return name_mismatch(&node, &body.node);
    }
    let logs = std::mem::take(&mut body.logs);
    let control = board::renew(job_id, body);
    // Only a node that still holds the lease may write into this job's log directory, and a node
    // being told to abandon has already had its job given away.
    if !control.abandon {
        crate::pnworker::link::logs::apply(job_id, &logs);
    }
    Json(control).into_response()
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

pub(super) async fn assets_manifest(Extension(auth): Extension<ApiAuth>) -> Response {
    if let Err(response) = require_link(&auth) {
        return response;
    }
    Json(assets::manifest()).into_response()
}

pub(super) async fn asset(
    Extension(auth): Extension<ApiAuth>,
    Path(hash): Path<String>,
) -> Response {
    if let Err(response) = require_link(&auth) {
        return response;
    }
    // Addressed by content, and only content the current manifest lists. A node cannot ask for a
    // path, which is what keeps this from being an arbitrary read of the coordinator's disk.
    let Some((entry, bytes)) = assets::read_asset(&hash) else {
        return (StatusCode::NOT_FOUND, "no such asset in the current manifest").into_response();
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (axum::http::header::CACHE_CONTROL, "no-store".to_string()),
            (
                axum::http::header::HeaderName::from_static("x-pandora-asset-name"),
                entry.name,
            ),
        ],
        bytes,
    )
        .into_response()
}

// A finished encode coming back from a node, for an HLS-only server whose playback URL has to live
// on this machine. It is the one large body the link carries, so it is streamed to disk rather than
// buffered: the alternative is holding an episode in memory.
pub(super) async fn output(
    Extension(auth): Extension<ApiAuth>,
    Path(job_id): Path<u64>,
    body: Body,
) -> Response {
    let node = match require_link(&auth) {
        Ok(node) => node,
        Err(response) => return response,
    };
    // A node may only deliver against a lease it actually holds. Without this, any link token
    // could write into any job's work directory.
    if board::node_for_job(job_id).as_deref() != Some(node.as_str()) {
        return (StatusCode::CONFLICT, "no such lease for this node").into_response();
    }
    let directory = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("DB")
        .join("work")
        .join(job_id.to_string())
        .join("work");
    if let Err(e) = tokio::fs::create_dir_all(&directory).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    // Written beside and renamed, so a transfer that dies halfway can never be mistaken for a
    // finished encode by the upload worker that is about to look for exactly this name.
    let target = directory.join("output.mp4");
    let temporary = directory.join("output.mp4.link-part");
    let file = match tokio::fs::File::create(&temporary).await {
        Ok(file) => file,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut writer = tokio::io::BufWriter::new(file);
    let mut stream = body.into_data_stream();
    let mut written = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(e) => {
                tokio::fs::remove_file(&temporary).await.ok();
                return (StatusCode::BAD_REQUEST, format!("upload stream failed: {e}"))
                    .into_response();
            }
        };
        if let Err(e) = writer.write_all(&chunk).await {
            tokio::fs::remove_file(&temporary).await.ok();
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
        written += chunk.len() as u64;
    }
    if let Err(e) = writer.flush().await {
        tokio::fs::remove_file(&temporary).await.ok();
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    drop(writer);
    if written == 0 {
        tokio::fs::remove_file(&temporary).await.ok();
        return (StatusCode::BAD_REQUEST, "empty output").into_response();
    }
    if let Err(e) = tokio::fs::rename(&temporary, &target).await {
        tokio::fs::remove_file(&temporary).await.ok();
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    println!("[link] {node} | job {job_id} output received ({written} bytes)");
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
