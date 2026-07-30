use std::time::Instant;

use async_zip::base::write::ZipFileWriter;
use async_zip::{Compression, ZipEntryBuilder};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Query},
    http::{HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use crate::kagami_trace::{MAX_ENCODED_BYTES, Trace, TraceOptions, TracePreset, trace_image};
use crate::libkagami::complex::types::AssTime;
use crate::libkagami::tracing::{TraceAssOptions, trace_to_ass};

pub(super) const ASS_REQUEST_LIMIT: usize = 64 * 1024 * 1024;
const DEFAULT_ASS_DURATION_CENTISECONDS: u64 = 500;
const MAX_ASS_CENTISECONDS: u64 = 92_159_999;
const INDEX_HTML: &str = include_str!("../../../../kagami-trace/web/index.html");

#[derive(Debug, Default, Deserialize)]
pub(super) struct TraceQuery {
    preset: Option<TracePreset>,
    color_count: Option<u16>,
    preserve_gradients: Option<bool>,
    color_smoothing: Option<u8>,
    path_simplify: Option<f32>,
    curve_fit: Option<f32>,
    corner_threshold: Option<f32>,
    min_area: Option<u32>,
    alpha_threshold: Option<u8>,
    max_dimension: Option<u32>,
    svg_seam_overlap: Option<f32>,
}

#[derive(Deserialize)]
pub(super) struct AssRequest {
    trace: Trace,
    filename: Option<String>,
    duration_centiseconds: Option<u64>,
    seam_overlap: Option<f32>,
}

#[derive(Serialize)]
struct TraceResponse {
    trace: Trace,
    svg: String,
    elapsed_ms: u128,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

pub fn standalone_router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route(
            "/api/trace",
            post(run_trace).layer(DefaultBodyLimit::max(MAX_ENCODED_BYTES)),
        )
        .route(
            "/api/ass",
            post(export_ass).layer(DefaultBodyLimit::max(ASS_REQUEST_LIMIT)),
        )
}

pub(super) async fn index() -> Response {
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Html(INDEX_HTML),
    )
        .into_response()
}

async fn health() -> &'static str {
    "ok"
}

pub(super) async fn run_trace(Query(query): Query<TraceQuery>, bytes: Bytes) -> Response {
    let preset = query.preset;
    let mut options = preset.map(TraceOptions::for_preset).unwrap_or_default();
    if let Some(value) = query.color_count {
        options.color_count = value;
    }
    if let Some(value) = query.preserve_gradients {
        options.preserve_gradients = value;
    }
    if let Some(value) = query.color_smoothing {
        options.color_smoothing = value;
    }
    if let Some(value) = query.path_simplify {
        options.path_simplify = value;
    }
    if let Some(value) = query.curve_fit {
        options.curve_fit = value;
    }
    if let Some(value) = query.corner_threshold {
        options.corner_threshold = value;
    }
    if let Some(value) = query.min_area {
        options.min_area = value;
    }
    if let Some(value) = query.alpha_threshold {
        options.alpha_threshold = value;
    }
    if let Some(value) = query.max_dimension {
        options.max_dimension = value;
    }
    let svg_seam_overlap = query.svg_seam_overlap.unwrap_or_else(|| match preset {
        Some(TracePreset::LogoUi) => 0.0,
        Some(TracePreset::Illustration) => 0.25,
        Some(TracePreset::Photo) => 0.5,
        Some(TracePreset::Gradient) => 0.25,
        None if options.preserve_gradients => 0.25,
        None if options.curve_fit > 0.0 => 0.25,
        None => 0.0,
    });
    if !svg_seam_overlap.is_finite() || !(0.0..=4.0).contains(&svg_seam_overlap) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "SVG seam overlap must be finite and between 0 and 4".to_string(),
        );
    }

    let result = tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let trace = trace_image(&bytes, &options)?;
        let svg = trace.to_svg_with_seam_overlap(svg_seam_overlap);
        Ok::<_, crate::kagami_trace::TraceError>(TraceResponse {
            trace,
            svg,
            elapsed_ms: started.elapsed().as_millis(),
        })
    })
    .await;

    match result {
        Ok(Ok(response)) => (
            StatusCode::OK,
            [(header::CACHE_CONTROL, "no-store")],
            Json(response),
        )
            .into_response(),
        Ok(Err(error)) => error_response(StatusCode::BAD_REQUEST, error.to_string()),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

pub(super) async fn export_ass(Json(request): Json<AssRequest>) -> Response {
    let duration = request
        .duration_centiseconds
        .unwrap_or(DEFAULT_ASS_DURATION_CENTISECONDS);
    if duration == 0 || duration > MAX_ASS_CENTISECONDS {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!("ASS duration must be between 1 and {MAX_ASS_CENTISECONDS} centiseconds"),
        );
    }
    let seam_overlap = request
        .seam_overlap
        .unwrap_or_else(|| TraceAssOptions::default().seam_overlap);
    let stem = sanitize_stem(request.filename.as_deref().unwrap_or("trace"));
    let ass_name = format!("{stem}.ass");
    let archive_name = format!("{stem}.zip");
    let title = stem.clone();
    let converted = tokio::task::spawn_blocking(move || {
        let ass = trace_to_ass(
            &request.trace,
            &TraceAssOptions {
                title,
                end: AssTime::from_centiseconds(duration),
                seam_overlap,
                ..TraceAssOptions::default()
            },
        )?;
        Ok::<String, String>(ass.stringify())
    })
    .await;
    let ass = match converted {
        Ok(Ok(ass)) => ass,
        Ok(Err(error)) => return error_response(StatusCode::BAD_REQUEST, error),
        Err(error) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, error.to_string());
        }
    };
    let archive = match zip_ass(&ass_name, ass.as_bytes()).await {
        Ok(archive) => archive,
        Err(error) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
    };

    let mut response = (StatusCode::OK, archive).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let disposition = format!("attachment; filename=\"{archive_name}\"");
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).unwrap(),
    );
    response
}

async fn zip_ass(filename: &str, ass: &[u8]) -> Result<Vec<u8>, String> {
    let mut writer = ZipFileWriter::new(Vec::new());
    let entry = ZipEntryBuilder::new(filename.to_string().into(), Compression::Deflate);
    writer
        .write_entry_whole(entry, ass)
        .await
        .map_err(|error| format!("could not add ASS to ZIP: {error}"))?;
    writer
        .close()
        .await
        .map_err(|error| format!("could not finish ASS ZIP: {error}"))
}

fn sanitize_stem(input: &str) -> String {
    let filename = input.rsplit(['/', '\\']).next().unwrap_or(input);
    let stem = filename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|stem| !stem.is_empty())
        .unwrap_or(filename);
    let mut sanitized = String::new();
    for character in stem.chars().take(80) {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            sanitized.push(character);
        } else if !sanitized.ends_with('_') {
            sanitized.push('_');
        }
    }
    let sanitized = sanitized.trim_matches('_');
    if sanitized.is_empty() {
        "trace".to_string()
    } else {
        sanitized.to_string()
    }
}

fn error_response(status: StatusCode, error: String) -> Response {
    (status, Json(ErrorResponse { error })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_are_reduced_to_safe_stems() {
        assert_eq!(sanitize_stem("logo.png"), "logo");
        assert_eq!(sanitize_stem("../bad name?.png"), "bad_name");
        assert_eq!(sanitize_stem("..."), "trace");
    }

    #[tokio::test]
    async fn ass_archives_always_contain_one_ass_file() {
        let archive = zip_ass("sample.ass", b"[Script Info]\nTitle: test\n")
            .await
            .unwrap();
        let zip = async_zip::base::read::mem::ZipFileReader::new(archive)
            .await
            .unwrap();

        assert_eq!(zip.file().entries().len(), 1);
        assert_eq!(
            zip.file().entries()[0].filename().as_str().unwrap(),
            "sample.ass"
        );
        let mut contents = String::new();
        zip.reader_with_entry(0)
            .await
            .unwrap()
            .read_to_string_checked(&mut contents)
            .await
            .unwrap();
        assert_eq!(contents, "[Script Info]\nTitle: test\n");
    }
}
