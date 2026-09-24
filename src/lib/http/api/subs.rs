use super::core::{
    ApiAuth, AppState, effective_server_id, row_is_visible, submit_with_progress,
};
use axum::{
    Json,
    extract::{Extension, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use crate::lib::p2p::nyaaise::nyaaise;
use crate::pnworker::core::{Job, JobType};
use crate::pnworker::subs_media::{is_live, media_dir, public_file_type, valid_token};

#[derive(Deserialize)]
pub(super) struct SubsMediaReq {
    #[serde(default)]
    torrent: Option<String>,
    #[serde(default)]
    probe_job_id: Option<String>,
    #[serde(default)]
    file_index: Option<u64>,
}

// Queues a link for the browser subtitle editor. It takes what `/encode` takes — a nyaa page, a
// `.torrent` URL, a magnet, a Drive or direct link — or a finished probe plus the file picked from
// it, exactly as `/jobs/pancode` does, so a season pack becomes one episode instead of a guess.
pub(super) async fn submit_media(
    State(st): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Json(req): Json<SubsMediaReq>,
) -> Response {
    let probe = match req.probe_job_id.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
        Some(raw) => {
            let Ok(probe_id) = raw.parse::<u64>() else {
                return (StatusCode::BAD_REQUEST, "probe_job_id must be a numeric string").into_response();
            };
            let row = match st.db.get_job(probe_id).await {
                Ok(Some(row)) if row_is_visible(&st, &auth, &row).await => row,
                Ok(_) => return (StatusCode::NOT_FOUND, "no such probe job").into_response(),
                Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            };
            if row.archived != 0 {
                return (StatusCode::CONFLICT, "probe job is no longer active").into_response();
            }
            if row.stage != 21 {
                return (StatusCode::CONFLICT, "probe job is not ready yet").into_response();
            }
            Some((probe_id, row.link))
        }
        None => None,
    };
    if probe.is_none() && req.file_index.is_some() {
        return (StatusCode::BAD_REQUEST, "file_index needs the probe_job_id it was picked from").into_response();
    }
    let link = match &probe {
        Some((_, link)) => link.clone(),
        None => req.torrent.as_deref().unwrap_or("").trim().to_string(),
    };
    if link.is_empty() {
        return (StatusCode::BAD_REQUEST, "torrent or probe_job_id is required").into_response();
    }
    let mut job = Job::new_api(
        st.api_author,
        0,
        JobType::SubsMedia,
        nyaaise(&link),
        vec![],
        "EN".to_string(),
        effective_server_id(&auth, None),
    );
    if let Some((probe_id, _)) = &probe {
        job.probe_job_id = Some(*probe_id);
        job.probe_file_index = req.file_index;
    }
    if let Some(index) = req.file_index {
        job.display_link = Some(format!("{} : {}", link, index));
    }
    let progress = json!({ "type": "subsmedia", "percent": 0, "torrent": link });
    submit_with_progress(&st, &auth, job, Some(progress)).await
}

// Serves one file of a prepared source. The token is the whole credential, so everything that is
// not a well-formed token naming a live directory and one of the generated file names is the same
// 404 — nothing here tells a guesser which part was wrong.
pub(super) async fn media_file(
    Path((token, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, "not found").into_response();
    if !valid_token(&token) {
        return not_found();
    }
    let Some(content_type) = public_file_type(&file) else {
        return not_found();
    };
    let directory = media_dir(&token.to_ascii_lowercase());
    if !is_live(&directory).await {
        return not_found();
    }
    let path = directory.join(&file);
    let mut response = if file == "video.mp4" {
        super::studio::stream_media(path, &headers).await
    } else {
        match tokio::fs::read(&path).await {
            Ok(bytes) => ([(header::CONTENT_TYPE, content_type)], bytes).into_response(),
            Err(_) => return not_found(),
        }
    };
    let out = response.headers_mut();
    out.insert(header::CACHE_CONTROL, "private, max-age=3600".parse().unwrap());
    out.insert("X-Content-Type-Options", "nosniff".parse().unwrap());
    out.insert("X-Robots-Tag", "noindex, nofollow, noarchive".parse().unwrap());
    out.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    response
}
