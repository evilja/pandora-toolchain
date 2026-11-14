// Caramel Analysis over the API: queue an analysis of a source link, read its report back, and
// read what the model and its sampling plan are, for clients that calibrate against it.

use super::core::{ApiAuth, AppState, effective_server_id, row_is_visible, submit_with_progress};
use axum::{
    Json,
    extract::{Extension, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use crate::lib::mpeg::caramel::{CARAMEL_ANALYSIS, Sampling, SamplingPlan};
use crate::lib::p2p::nyaaise::nyaaise;
use crate::pnworker::caramel::{CaramelJobRequest, VerifySettings, read_report};
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
    // `true` for the default verification, or `{ crfs, clip_seconds, max_clips }`.
    #[serde(default)]
    verify: Option<serde_json::Value>,
    #[serde(default)]
    sampling: Option<Sampling>,
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
            Ok(Some(VerifySettings {
                crfs,
                clip_seconds: fields.get("clip_seconds").and_then(|value| value.as_f64()),
                max_clips: fields.get("max_clips").and_then(|value| value.as_u64()).map(|n| n as usize),
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
        sampling: req.sampling.unwrap_or_default(),
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

// What a report was produced by: the model version and the sampling plans it uses by default.
pub(super) async fn model() -> Response {
    Json(json!({
        "model": caramel_kernel::MODEL_VERSION,
        "aot_crf": CARAMEL_ANALYSIS,
        "sampling": {
            "file": SamplingPlan::default_for(false),
            "range": SamplingPlan::default_for(true),
        },
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_accepts_a_flag_or_settings() {
        assert!(parse_verify(None).unwrap().is_none());
        assert!(parse_verify(Some(&json!(false))).unwrap().is_none());
        assert!(parse_verify(Some(&json!(true))).unwrap().unwrap().crfs.is_empty());
        let settings = parse_verify(Some(&json!({ "crfs": [16, 18.5], "max_clips": 3 }))).unwrap().unwrap();
        assert_eq!(settings.crfs, vec![16.0, 18.5]);
        assert_eq!(settings.max_clips, Some(3));
        assert!(parse_verify(Some(&json!({ "crfs": ["x"] }))).is_err());
        assert!(parse_verify(Some(&json!("yes"))).is_err());
    }

    #[test]
    fn presets_are_named_the_way_edit_names_them() {
        assert_eq!(canonical_preset("standard").as_deref(), Some("standard"));
        assert_eq!(canonical_preset("very_slow").as_deref(), Some("veryslow"));
        assert!(canonical_preset("1440p").is_none());
    }
}
