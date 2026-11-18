// Caramel Analysis over the API: queue an analysis of a source link, read its scene report back,
// and read what the model and its scene settings are, for clients that calibrate against it.

use super::core::{ApiAuth, AppState, effective_server_id, require_privileged, row_is_visible, submit_with_progress};
use axum::{
    Json,
    extract::{Extension, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use crate::lib::mpeg::caramel::{CARAMEL_ANALYSIS, VerifySample};
use crate::lib::p2p::nyaaise::nyaaise;
use crate::lib::env::core::upsert_env;
use crate::lib::env::standard::{CARAMEL_RESEARCH, CARAMEL_RESEARCH_CRFS, CARAMEL_RESEARCH_PRESET, CARAMEL_RESEARCH_RETRAIN, CARAMEL_RESEARCH_SCENES, ENV_PATH};
use crate::lib::mpeg::caramel_train::DATASET_DIRECTORY;
use crate::pnworker::research;
use crate::pnworker::caramel::{CaramelJobRequest, SceneOverrides, VerifySettings, read_report};
use crate::pnworker::core::{Concat, Job, JobType};
use crate::pnworker::server_effects::{load_server_settings, preset_from_name};

#[derive(Deserialize)]
pub(super) struct CaramelReq {
    #[serde(default)]
    torrent: Option<String>,
    #[serde(default)]
    probe_job_id: Option<String>,
    #[serde(default)]
    file_index: Option<u64>,
    #[serde(default)]
    server_id: Option<u64>,
    // A preset name as `/edit preset` takes it; the server's own preset when absent.
    #[serde(default)]
    preset: Option<String>,
    #[serde(default)]
    anchor: Option<f32>,
    // `true` for the default verification, or `{ crfs, clip_seconds, max_scenes, sample }`, where
    // `crfs` are the fixed CRFs the scene CRFs are compared with (the anchor when absent) and
    // `sample` says which scenes are measured.
    #[serde(default)]
    verify: Option<serde_json::Value>,
    // `{ analysis_frames, min_scene_frames, max_scene_frames }`, each optional.
    #[serde(default)]
    scenes: Option<SceneOverrides>,
}

fn parse_verify(value: Option<&serde_json::Value>) -> Result<Option<VerifySettings>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(false) => Ok(None),
        serde_json::Value::Bool(true) => Ok(Some(VerifySettings::default())),
        serde_json::Value::Object(fields) => {
            let crfs = match fields.get("crfs") {
                None | Some(serde_json::Value::Null) => Vec::new(),
                Some(serde_json::Value::Array(values)) => values
                    .iter()
                    .map(|value| value.as_f64().map(|crf| crf as f32).ok_or("verify.crfs must be numbers"))
                    .collect::<Result<_, _>>()?,
                Some(_) => return Err("verify.crfs must be an array of numbers".to_string()),
            };
            let sample = match fields.get("sample") {
                None | Some(serde_json::Value::Null) => VerifySample::default(),
                Some(value) => value
                    .as_str()
                    .and_then(VerifySample::from_name)
                    .ok_or("verify.sample must be \"moved\" or \"spread\"")?,
            };
            Ok(Some(VerifySettings {
                crfs,
                sample,
                clip_seconds: fields.get("clip_seconds").and_then(|value| value.as_f64()),
                max_scenes: fields
                    .get("max_scenes")
                    .or_else(|| fields.get("max_clips"))
                    .and_then(|value| value.as_u64())
                    .map(|n| n as usize),
            }))
        }
        _ => Err("verify must be a boolean or an object".to_string()),
    }
}

// The canonical name of the preset to anchor at, or None when the name is not a preset.
pub(crate) fn canonical_preset(name: &str) -> Option<String> {
    preset_from_name(name, Concat::NONE).and_then(|preset| preset.name())
}

pub(super) async fn submit(
    State(st): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Json(req): Json<CaramelReq>,
) -> Response {
    let verify = match parse_verify(req.verify.as_ref()) {
        Ok(verify) => verify,
        Err(reason) => return (StatusCode::BAD_REQUEST, reason).into_response(),
    };
    if req.anchor.is_some_and(|anchor| !anchor.is_finite() || !(0.0..=51.0).contains(&anchor)) {
        return (StatusCode::BAD_REQUEST, "anchor must be a CRF between 0 and 51").into_response();
    }
    let server_id = effective_server_id(&auth, req.server_id);
    let preset = match req.preset.as_deref().map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => match canonical_preset(name) {
            Some(canonical) => canonical,
            None => return (StatusCode::BAD_REQUEST, "no such preset").into_response(),
        },
        None => load_server_settings(server_id).preset.name().unwrap_or_else(|| "standard".to_string()),
    };

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
        JobType::Caramel,
        nyaaise(&link),
        vec![],
        "EN".to_string(),
        server_id,
    );
    if let Some((probe_id, _)) = &probe {
        job.probe_job_id = Some(*probe_id);
        job.probe_file_index = req.file_index;
    }
    if let Some(index) = req.file_index {
        job.display_link = Some(format!("{} : {}", link, index));
    }
    job.caramel = Some(CaramelJobRequest {
        preset: Some(preset.clone()),
        anchor: req.anchor,
        verify,
        scenes: req.scenes.unwrap_or_default(),
        fast: false,
        host: false,
    });
    let progress = json!({ "type": "caramel", "phase": "queued", "torrent": link, "preset": preset });
    submit_with_progress(&st, &auth, job, Some(progress)).await
}

// The stored report of a finished analysis. Same visibility as the job itself.
pub(super) async fn report(
    State(st): State<AppState>,
    Extension(auth): Extension<ApiAuth>,
    Path(id): Path<u64>,
) -> Response {
    let row = match st.db.get_job(id).await {
        Ok(Some(row)) if row_is_visible(&st, &auth, &row).await => row,
        Ok(_) => return (StatusCode::NOT_FOUND, "no such job").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if row.job_type != JobType::Caramel as u16 as i64 {
        return (StatusCode::NOT_FOUND, "not a Caramel Analysis job").into_response();
    }
    match read_report(id).await {
        Some(report) => Json(report).into_response(),
        None => (StatusCode::NOT_FOUND, "no report for this job (still running, failed, or expired)").into_response(),
    }
}

// What a report was produced by: the model version and the scene settings every encode uses.
pub(super) async fn model() -> Response {
    // The research worker's status reads the dataset from disk: megabytes of it, off this thread.
    let research = tokio::task::spawn_blocking(crate::pnworker::research::status).await.unwrap_or(serde_json::Value::Null);
    Json(json!({
        "model": crate::lib::mpeg::caramel_train::installed_version(),
        // What the idle research worker has gathered and what its last training said.
        "research": research,
        "aot_crf": CARAMEL_ANALYSIS,
        "crf_mode": CARAMEL_ANALYSIS,
        "scenes": caramel_kernel::SceneConfig::default(),
    }))
    .into_response()
}

// The research worker as its operator sees it: its status, its settings, and the releases queued
// for it with their links. Privileged: the queue names what this machine downloads.
pub(super) async fn research_status(Extension(auth): Extension<ApiAuth>) -> Response {
    if let Err(response) = require_privileged(&auth) {
        return response;
    }
    let (status, requests) = tokio::task::spawn_blocking(|| (research::status(), research::load_requests())).await.unwrap_or_default();
    let config = research::research_config(&crate::lib::env::core::get_pandora_env());
    Json(json!({
        "status": status,
        "settings": { "enabled": config.enabled, "scenes": config.scenes, "retrain": config.retrain, "preset": config.preset, "crfs": config.crfs },
        "requests": requests,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct ResearchSettingsReq {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    scenes: Option<u64>,
    #[serde(default)]
    retrain: Option<u64>,
    #[serde(default)]
    preset: Option<String>,
    // The CRFs every scene is encoded at; an empty list goes back to searching each scene.
    #[serde(default)]
    crfs: Option<Vec<f64>>,
}

// The `env.pandora` lines a settings change writes, or why it is refused. Nothing is written
// unless every field is valid. The feed is not here: what a machine downloads by itself is set on
// the machine.
fn research_settings_lines(req: &ResearchSettingsReq) -> Result<Vec<(&'static str, String)>, String> {
    let mut lines = Vec::new();
    if let Some(enabled) = req.enabled {
        lines.push((CARAMEL_RESEARCH, if enabled { "on" } else { "off" }.to_string()));
    }
    if let Some(scenes) = req.scenes {
        if !(4..=400).contains(&scenes) {
            return Err("scenes must be between 4 and 400".to_string());
        }
        lines.push((CARAMEL_RESEARCH_SCENES, scenes.to_string()));
    }
    if let Some(retrain) = req.retrain {
        if !(20..=100_000).contains(&retrain) {
            return Err("retrain must be between 20 and 100000".to_string());
        }
        lines.push((CARAMEL_RESEARCH_RETRAIN, retrain.to_string()));
    }
    if let Some(preset) = req.preset.as_deref() {
        lines.push((CARAMEL_RESEARCH_PRESET, canonical_preset(preset.trim()).ok_or("no such preset")?));
    }
    if let Some(crfs) = &req.crfs {
        if crfs.len() > 12 || crfs.iter().any(|crf| !crf.is_finite() || !(0.0..=51.0).contains(crf)) {
            return Err("crfs must be at most 12 CRFs between 0 and 51".to_string());
        }
        let text = crfs.iter().map(|crf| crf.to_string()).collect::<Vec<_>>().join(",");
        lines.push((CARAMEL_RESEARCH_CRFS, research::parse_crfs(&text).iter().map(|crf| crf.to_string()).collect::<Vec<_>>().join(",")));
    }
    Ok(lines)
}

// Changes the research worker's settings. It reads them every 20 seconds, so they take effect
// without a restart.
pub(super) async fn research_settings(Extension(auth): Extension<ApiAuth>, Json(req): Json<ResearchSettingsReq>) -> Response {
    if let Err(response) = require_privileged(&auth) {
        return response;
    }
    let lines = match research_settings_lines(&req) {
        Ok(lines) => lines,
        Err(reason) => return (StatusCode::BAD_REQUEST, reason).into_response(),
    };
    for (key, value) in &lines {
        if let Err(error) = upsert_env(ENV_PATH, key, value) {
            return (StatusCode::INTERNAL_SERVER_ERROR, error).into_response();
        }
    }
    let config = research::research_config(&crate::lib::env::core::get_pandora_env());
    Json(json!({ "enabled": config.enabled, "scenes": config.scenes, "retrain": config.retrain, "preset": config.preset, "crfs": config.crfs })).into_response()
}

#[derive(Deserialize)]
pub(super) struct ResearchSourceReq {
    torrent: String,
    #[serde(default)]
    file_index: Option<u64>,
    series: String,
}

// Queues a release for the research worker: it is downloaded and measured when the machine is
// idle, as the series named here, and deleted once measured.
pub(super) async fn research_add_source(Extension(auth): Extension<ApiAuth>, Json(req): Json<ResearchSourceReq>) -> Response {
    if let Err(response) = require_privileged(&auth) {
        return response;
    }
    match tokio::task::spawn_blocking(move || research::add_request(&req.torrent, req.file_index, &req.series)).await {
        Ok(Ok(request)) => (StatusCode::ACCEPTED, Json(json!(request))).into_response(),
        Ok(Err(reason)) => (StatusCode::BAD_REQUEST, reason).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

pub(super) async fn research_remove_source(Extension(auth): Extension<ApiAuth>, Path(id): Path<String>) -> Response {
    if let Err(response) = require_privileged(&auth) {
        return response;
    }
    if !research::is_request_id(&id) {
        return (StatusCode::NOT_FOUND, "no such request").into_response();
    }
    match tokio::task::spawn_blocking(move || research::remove_request(&id)).await {
        Ok(Ok(true)) => Json(json!({ "removed": true })).into_response(),
        Ok(Ok(false)) => (StatusCode::NOT_FOUND, "no such request").into_response(),
        Ok(Err(reason)) => (StatusCode::CONFLICT, reason).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

// A dataset file's name as the routes accept it: a plain `.jsonl` name in the dataset directory,
// never a hidden one and never a path.
fn dataset_name_ok(name: &str) -> bool {
    name.len() <= 200
        && name.ends_with(".jsonl")
        && !name.starts_with('.')
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

// A model trained elsewhere, installed here: the other half of copying the dataset out. The body is
// a model file as `pnmpeg --caramel-train -o` writes it. It is read whole before anything is
// written, so a truncated or foreign file changes nothing; one of another family is refused because
// its outputs mean something else to this build. Privileged: it changes every encode that follows.
pub(super) async fn install_model(Extension(auth): Extension<ApiAuth>, body: String) -> Response {
    if let Err(response) = require_privileged(&auth) {
        return response;
    }
    let result = tokio::task::spawn_blocking(move || {
        let model = caramel_kernel::curve::Model::from_text(&body).map_err(|error| (StatusCode::BAD_REQUEST, format!("not a model file: {error}")))?;
        if !model.version.starts_with(&format!("{}.", crate::lib::mpeg::caramel_train::MODEL_FAMILY)) {
            return Err((StatusCode::BAD_REQUEST, format!("`{}` is not a {} model", model.version, crate::lib::mpeg::caramel_train::MODEL_FAMILY)));
        }
        crate::lib::mpeg::caramel_train::install(&model).map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
        Ok(model.version.clone())
    })
    .await
    .unwrap_or_else(|_| Err((StatusCode::INTERNAL_SERVER_ERROR, "install failed".to_string())));
    match result {
        Ok(version) => Json(json!({ "model": version, "installed": crate::lib::mpeg::caramel_train::installed_models() })).into_response(),
        Err((status, message)) => (status, message).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct ActiveModelReq {
    version: Option<String>,
}

// Points new encodes at an installed model, or with `null` at the built-in one: an install undone
// without having to keep the old file anywhere but here.
pub(super) async fn activate_model(Extension(auth): Extension<ApiAuth>, Json(req): Json<ActiveModelReq>) -> Response {
    if let Err(response) = require_privileged(&auth) {
        return response;
    }
    let result = tokio::task::spawn_blocking(move || crate::lib::mpeg::caramel_train::point_at(req.version.as_deref()))
        .await
        .unwrap_or_else(|_| Err("activation failed".to_string()));
    match result {
        Ok(version) => Json(json!({ "model": version, "installed": crate::lib::mpeg::caramel_train::installed_models() })).into_response(),
        Err(message) => (StatusCode::NOT_FOUND, message).into_response(),
    }
}

// The dataset's files: what the lab has measured on this machine, to copy and merge elsewhere.
pub(super) async fn dataset_list(Extension(auth): Extension<ApiAuth>) -> Response {
    if let Err(response) = require_privileged(&auth) {
        return response;
    }
    let files = tokio::task::spawn_blocking(|| {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(DATASET_DIRECTORY).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() || !dataset_name_ok(&name) {
                continue;
            }
            let scenes = std::fs::read(entry.path()).map(|bytes| bytes.iter().filter(|byte| **byte == b'\n').count()).unwrap_or(0);
            let modified = meta.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map(|elapsed| elapsed.as_secs()).unwrap_or(0);
            files.push(json!({ "name": name, "bytes": meta.len(), "scenes": scenes, "modified": modified }));
        }
        files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        files
    })
    .await
    .unwrap_or_default();
    Json(json!({ "files": files })).into_response()
}

pub(super) async fn dataset_file(Extension(auth): Extension<ApiAuth>, Path(name): Path<String>) -> Response {
    if let Err(response) = require_privileged(&auth) {
        return response;
    }
    if !dataset_name_ok(&name) {
        return (StatusCode::NOT_FOUND, "no such dataset file").into_response();
    }
    match tokio::fs::read(std::path::Path::new(DATASET_DIRECTORY).join(&name)).await {
        Ok(bytes) => ([(axum::http::header::CONTENT_TYPE, "application/x-ndjson")], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no such dataset file").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_accepts_a_flag_or_settings() {
        assert!(parse_verify(None).unwrap().is_none());
        assert!(parse_verify(Some(&json!(false))).unwrap().is_none());
        assert!(parse_verify(Some(&json!(true))).unwrap().unwrap().crfs.is_empty());
        let settings = parse_verify(Some(&json!({ "crfs": [16, 18.5], "max_scenes": 3 }))).unwrap().unwrap();
        assert_eq!(settings.crfs, vec![16.0, 18.5]);
        assert_eq!(settings.max_scenes, Some(3));
        // The name it had when verification sampled clips rather than scenes still works.
        assert_eq!(parse_verify(Some(&json!({ "max_clips": 2 }))).unwrap().unwrap().max_scenes, Some(2));
        assert!(parse_verify(Some(&json!({ "crfs": ["x"] }))).is_err());
        assert_eq!(settings.sample, VerifySample::Moved);
        let spread = parse_verify(Some(&json!({ "sample": "spread", "max_scenes": 48 }))).unwrap().unwrap();
        assert_eq!(spread.sample, VerifySample::Spread);
        assert!(parse_verify(Some(&json!({ "sample": "random" }))).is_err());
        assert!(parse_verify(Some(&json!("yes"))).is_err());
    }

    #[test]
    fn dataset_names_are_plain_files() {
        assert!(dataset_name_ok("akira.jsonl"));
        assert!(dataset_name_ok("g-some-show-03-1080p-abcd-mkv-1a2b3c4d.jsonl"));
        assert!(!dataset_name_ok(".caramel-lab-akira.jsonl"));
        assert!(!dataset_name_ok("../env.jsonl"));
        assert!(!dataset_name_ok("akira.json"));
        assert!(!dataset_name_ok("a/b.jsonl"));
    }

    #[test]
    fn research_settings_are_checked_before_any_is_written() {
        let req = |value: serde_json::Value| serde_json::from_value::<ResearchSettingsReq>(value).unwrap();
        let lines = research_settings_lines(&req(json!({ "enabled": true, "scenes": 32, "crfs": [31, 11, 15] }))).unwrap();
        assert_eq!(lines, vec![(CARAMEL_RESEARCH, "on".to_string()), (CARAMEL_RESEARCH_SCENES, "32".to_string()), (CARAMEL_RESEARCH_CRFS, "11,15,31".to_string())]);
        assert_eq!(research_settings_lines(&req(json!({ "crfs": [] }))).unwrap(), vec![(CARAMEL_RESEARCH_CRFS, String::new())]);
        assert!(research_settings_lines(&req(json!({ "enabled": true, "scenes": 2 }))).is_err());
        assert!(research_settings_lines(&req(json!({ "crfs": [60] }))).is_err());
        assert!(research_settings_lines(&req(json!({ "preset": "1440p" }))).is_err());
        assert!(research_settings_lines(&req(json!({}))).unwrap().is_empty());
    }

    #[test]
    fn presets_are_named_the_way_edit_names_them() {
        assert_eq!(canonical_preset("standard").as_deref(), Some("standard"));
        assert_eq!(canonical_preset("very_slow").as_deref(), Some("veryslow"));
        assert!(canonical_preset("1440p").is_none());
    }
}
