use axum::body::Body;
use axum::extract::Path;
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::{Path as FsPath, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, ReadBuf};
use tokio_util::io::ReaderStream;

use super::client::Config;
use super::observe::{bytes_label, duration_label, info, token_tag, trace, warn};

static TRANSFERS: OnceLock<RwLock<HashMap<String, TransferRecord>>> = OnceLock::new();

#[derive(Clone)]
struct TransferRecord {
    path: PathBuf,
    filename: String,
    content_type: String,
    size: u64,
    expires_at: Instant,
    bytes_served: Arc<AtomicU64>,
}

pub struct TransferLease {
    inner: Arc<LeaseInner>,
    url: String,
    size: u64,
    bytes_served: Arc<AtomicU64>,
}

struct LeaseInner {
    token: String,
    size: u64,
    bytes_served: Arc<AtomicU64>,
    created: Instant,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        if let Ok(mut transfers) = registry().write() {
            transfers.remove(&self.token);
        }
        let served = self.bytes_served.load(Ordering::Relaxed);
        info(
            &format!("xfer {}", token_tag(&self.token)),
            format!(
                "capability revoked after {}: {} of {} served",
                duration_label(self.created.elapsed()),
                bytes_label(served),
                bytes_label(self.size)
            ),
        );
    }
}

impl TransferLease {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn bytes_served(&self) -> u64 {
        self.bytes_served.load(Ordering::Relaxed).min(self.size)
    }
}

impl Clone for TransferLease {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            url: self.url.clone(),
            size: self.size,
            bytes_served: self.bytes_served.clone(),
        }
    }
}

pub async fn register_transfer(
    path: &FsPath,
    filename: &str,
    content_type: &str,
    config: &Config,
) -> Result<TransferLease, TransferError> {
    let db_root = tokio::fs::canonicalize("DB")
        .await
        .map_err(|_| TransferError::new("Pandora DB root is unavailable"))?;
    register_transfer_under(
        path,
        &db_root,
        filename,
        content_type,
        config,
        config.transfer_ttl(),
    )
    .await
}

async fn register_transfer_under(
    path: &FsPath,
    allowed_root: &FsPath,
    filename: &str,
    content_type: &str,
    config: &Config,
    ttl: Duration,
) -> Result<TransferLease, TransferError> {
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|_| TransferError::new("upload source file is unavailable"))?;
    if !canonical.starts_with(allowed_root) {
        return Err(TransferError::new(
            "upload source is outside Pandora's DB directory",
        ));
    }
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|_| TransferError::new("upload source metadata is unavailable"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(TransferError::new("upload source is not a non-empty file"));
    }
    let filename = validate_filename(filename)?;
    let content_type = validate_content_type(content_type);
    let bytes_served = Arc::new(AtomicU64::new(0));
    let expires_at = Instant::now() + ttl;

    let token = loop {
        let token = random_token()?;
        let mut transfers = registry()
            .write()
            .map_err(|_| TransferError::new("transfer registry is unavailable"))?;
        transfers.retain(|_, record| record.expires_at > Instant::now());
        if transfers.contains_key(&token) {
            continue;
        }
        transfers.insert(
            token.clone(),
            TransferRecord {
                path: canonical.clone(),
                filename: filename.clone(),
                content_type: content_type.clone(),
                size: metadata.len(),
                expires_at,
                bytes_served: bytes_served.clone(),
            },
        );
        break token;
    };

    let mut url = config.public_url().clone();
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| TransferError::new("Lumiere public URL cannot be used for file paths"))?;
        segments.pop_if_empty();
        segments.push("lumiere");
        segments.push("v1");
        segments.push("files");
        segments.push(&token);
        segments.push(&filename);
    }
    info(
        &format!("xfer {}", token_tag(&token)),
        format!(
            "published {} as {} ({}, {}) for {}",
            canonical.display(),
            filename,
            bytes_label(metadata.len()),
            content_type,
            duration_label(ttl)
        ),
    );
    Ok(TransferLease {
        inner: Arc::new(LeaseInner {
            token,
            size: metadata.len(),
            bytes_served: bytes_served.clone(),
            created: Instant::now(),
        }),
        url: url.to_string(),
        size: metadata.len(),
        bytes_served,
    })
}

pub async fn serve_transfer(
    Path((token, filename)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let scope = format!("xfer {}", token_tag(&token));
    let range_header = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-")
        .to_string();
    let client = client_label(&headers);
    trace(
        &scope,
        format!("{method} {filename} range={range_header} from {client}"),
    );
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        warn(
            &scope,
            format!("rejected a malformed capability token from {client}"),
        );
        return not_found();
    }
    let record = {
        let transfers = match registry().read() {
            Ok(transfers) => transfers,
            Err(_) => {
                warn(&scope, "transfer registry lock is poisoned");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "transfer registry unavailable",
                )
                    .into_response();
            }
        };
        transfers.get(&token).cloned()
    };
    let Some(record) = record else {
        warn(
            &scope,
            format!(
                "404 for {client}: no such capability (already finished, or pndc restarted since it was issued)"
            ),
        );
        return not_found();
    };
    if record.expires_at <= Instant::now() {
        if let Ok(mut transfers) = registry().write() {
            transfers.remove(&token);
        }
        warn(
            &scope,
            format!("404 for {client}: capability expired before the host fetched it"),
        );
        return not_found();
    }
    if record.filename != filename {
        warn(
            &scope,
            format!(
                "404 for {client}: asked for {filename}, capability holds {}",
                record.filename
            ),
        );
        return not_found();
    }
    if method != Method::GET && method != Method::HEAD {
        warn(&scope, format!("{client} used unsupported method {method}"));
        return (StatusCode::METHOD_NOT_ALLOWED, "method not allowed").into_response();
    }

    let range = match headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
    {
        Some(raw) => match parse_byte_range(raw, record.size) {
            Some(range) => Some(range),
            None => {
                warn(
                    &scope,
                    format!(
                        "416 for {client}: unusable range {raw} against {}",
                        bytes_label(record.size)
                    ),
                );
                return base_response(StatusCode::RANGE_NOT_SATISFIABLE, &record)
                    .header(header::CONTENT_RANGE, format!("bytes */{}", record.size))
                    .body(Body::empty())
                    .unwrap();
            }
        },
        None => None,
    };
    let (start, end, status) = range
        .map(|(start, end)| (start, end, StatusCode::PARTIAL_CONTENT))
        .unwrap_or((0, record.size - 1, StatusCode::OK));
    let content_length = end - start + 1;
    let mut builder = base_response(status, &record).header(header::CONTENT_LENGTH, content_length);
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", record.size),
        );
    }
    if method == Method::HEAD {
        info(
            &scope,
            format!(
                "{client} probed the file with HEAD ({})",
                bytes_label(record.size)
            ),
        );
        return builder.body(Body::empty()).unwrap();
    }

    let mut file = match tokio::fs::File::open(&record.path).await {
        Ok(file) => file,
        Err(error) => {
            warn(
                &scope,
                format!("source {} disappeared: {error}", record.path.display()),
            );
            return not_found();
        }
    };
    if start > 0 && file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        warn(&scope, format!("failed to seek to {start} for {client}"));
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to seek transfer source",
        )
            .into_response();
    }
    info(
        &scope,
        format!(
            "{client} is downloading bytes {start}-{end} ({} of {})",
            bytes_label(content_length),
            bytes_label(record.size)
        ),
    );
    let reader = CountingReader {
        inner: file.take(content_length),
        bytes_served: record.bytes_served,
        scope,
        client,
        expected: content_length,
        streamed: 0,
        started: Instant::now(),
    };
    let stream = ReaderStream::with_capacity(reader, 256 * 1024);
    builder.body(Body::from_stream(stream)).unwrap()
}

fn base_response(status: StatusCode, record: &TransferRecord) -> axum::http::response::Builder {
    Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, record.content_type.as_str())
        .header(header::CACHE_CONTROL, "private, no-store, max-age=0")
        .header("X-Content-Type-Options", "nosniff")
        .header("X-Robots-Tag", "noindex, nofollow, noarchive")
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "transfer not found").into_response()
}

fn registry() -> &'static RwLock<HashMap<String, TransferRecord>> {
    TRANSFERS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn random_token() -> Result<String, TransferError> {
    crate::lib::secret::random_hex_token()
        .map_err(|_| TransferError::new("secure transfer token generation failed"))
}

fn validate_filename(filename: &str) -> Result<String, TransferError> {
    let filename = filename.trim();
    if filename.is_empty()
        || filename.chars().count() > 180
        || filename.contains(['/', '\\'])
        || filename.chars().any(char::is_control)
        || matches!(filename, "." | "..")
    {
        return Err(TransferError::new("upload filename is unsafe"));
    }
    Ok(filename.to_string())
}

fn validate_content_type(content_type: &str) -> String {
    let value = content_type.trim();
    if value.is_empty()
        || value.len() > 100
        || value.chars().any(|character| character.is_control())
    {
        "application/octet-stream".to_string()
    } else {
        value.to_string()
    }
}

fn parse_byte_range(raw: &str, file_size: u64) -> Option<(u64, u64)> {
    let value = raw.trim().strip_prefix("bytes=")?;
    if value.contains(',') || file_size == 0 {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?.min(file_size);
        if suffix == 0 {
            return None;
        }
        return Some((file_size - suffix, file_size - 1));
    }
    let start = start.parse::<u64>().ok()?;
    if start >= file_size {
        return None;
    }
    let end = if end.is_empty() {
        file_size - 1
    } else {
        end.parse::<u64>().ok()?.min(file_size - 1)
    };
    (end >= start).then_some((start, end))
}

struct CountingReader<R> {
    inner: R,
    bytes_served: Arc<AtomicU64>,
    scope: String,
    client: String,
    expected: u64,
    streamed: u64,
    started: Instant,
}

impl<R: AsyncRead + Unpin> AsyncRead for CountingReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(context, buffer);
        let read = buffer.filled().len().saturating_sub(before);
        if read > 0 {
            self.bytes_served.fetch_add(read as u64, Ordering::Relaxed);
            self.streamed += read as u64;
        }
        result
    }
}

// A provider that hangs up mid-file is the difference between "the host is slow"
// and "the host gave up", and nothing else in the pipeline can see it.
impl<R> Drop for CountingReader<R> {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        if self.streamed >= self.expected {
            info(
                &self.scope,
                format!(
                    "{} finished its download: {} in {}",
                    self.client,
                    bytes_label(self.streamed),
                    duration_label(elapsed)
                ),
            );
        } else {
            warn(
                &self.scope,
                format!(
                    "{} disconnected after {} of {} in {}",
                    self.client,
                    bytes_label(self.streamed),
                    bytes_label(self.expected),
                    duration_label(elapsed)
                ),
            );
        }
    }
}

// Provider fetchers are only identifiable by their user agent and forwarded IP;
// both are what tells a real provider pull apart from a Cloudflare challenge or
// an unrelated crawler hitting the file hostname.
fn client_label(headers: &HeaderMap) -> String {
    let agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(60).collect::<String>())
        .unwrap_or_else(|| "<no user agent>".to_string());
    let address = headers
        .get("CF-Connecting-IP")
        .or_else(|| headers.get("X-Forwarded-For"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .unwrap_or("unknown ip");
    format!("{address} \"{agent}\"")
}

#[derive(Debug)]
pub struct TransferError {
    message: String,
}

impl TransferError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for TransferError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for TransferError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ranges_support_bounded_open_and_suffix_forms() {
        assert_eq!(parse_byte_range("bytes=2-5", 10), Some((2, 5)));
        assert_eq!(parse_byte_range("bytes=7-", 10), Some((7, 9)));
        assert_eq!(parse_byte_range("bytes=-3", 10), Some((7, 9)));
    }

    #[test]
    fn byte_ranges_reject_multiple_or_unsatisfiable_ranges() {
        assert_eq!(parse_byte_range("bytes=0-1,4-5", 10), None);
        assert_eq!(parse_byte_range("bytes=10-", 10), None);
        assert_eq!(parse_byte_range("items=0-1", 10), None);
    }

    #[test]
    fn filenames_cannot_escape_the_capability_route() {
        assert!(validate_filename("episode 01.mp4").is_ok());
        assert!(validate_filename("../secret").is_err());
        assert!(validate_filename("folder/video.mp4").is_err());
    }

    #[tokio::test]
    async fn capability_handler_streams_only_the_requested_range() {
        let token = "a".repeat(64);
        let filename = "test.mp4".to_string();
        let path =
            std::env::temp_dir().join(format!("lumiere-transfer-{}.bin", std::process::id()));
        tokio::fs::write(&path, b"0123456789").await.unwrap();
        registry().write().unwrap().insert(
            token.clone(),
            TransferRecord {
                path: path.clone(),
                filename: filename.clone(),
                content_type: "video/mp4".to_string(),
                size: 10,
                expires_at: Instant::now() + Duration::from_secs(60),
                bytes_served: Arc::new(AtomicU64::new(0)),
            },
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=2-5".parse().unwrap());
        let response = serve_transfer(Path((token.clone(), filename)), Method::GET, headers).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
        let body = axum::body::to_bytes(response.into_body(), 16)
            .await
            .unwrap();
        assert_eq!(&body[..], b"2345");
        registry().write().unwrap().remove(&token);
        tokio::fs::remove_file(path).await.ok();
    }
}
