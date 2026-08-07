use md5::Context as Md5Context;
use reqwest::{Client as HttpClient, StatusCode, Url, header};
use serde::Deserialize;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::mpsc::UnboundedSender;

use super::client::LumiereClient;
use super::protocol::{
    DriveCandidate, DriveSessionRequest, DriveSessionResponse, RemoteProvider, RemoteStartRequest,
    RemoteState,
};
use super::transfer::register_transfer;

const DRIVE_CHUNK_SIZE: usize = 8 * 1024 * 1024;
const DRIVE_RETRY_LIMIT: u8 = 5;
const REMOTE_STATUS_ERROR_LIMIT: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadProgress {
    pub sent: u64,
    pub total: u64,
}

pub struct DriveUploadSpec {
    pub path: PathBuf,
    pub request_id: String,
    pub candidates: Vec<DriveCandidate>,
    pub content_type: String,
    pub cancel_file: Option<PathBuf>,
}

pub struct RemoteUploadSpec {
    pub path: PathBuf,
    pub request_id: String,
    pub provider: RemoteProvider,
    pub filename: String,
    pub content_type: String,
    pub cancel_file: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct DriveUploadResult {
    pub url: String,
    pub file_id: String,
    pub parent_id: String,
    pub profile: String,
    pub root: String,
    pub candidate_index: usize,
    pub delete_token: String,
}

#[derive(Clone, Debug)]
pub struct RemoteUploadResult {
    pub url: String,
    pub file_code: String,
}

impl LumiereClient {
    pub async fn upload_drive(
        &self,
        spec: DriveUploadSpec,
        progress: Option<UnboundedSender<UploadProgress>>,
    ) -> Result<DriveUploadResult, UploadError> {
        validate_drive_spec(&spec)?;
        let metadata = tokio::fs::metadata(&spec.path)
            .await
            .map_err(|_| UploadError::failed("Drive upload source is unavailable"))?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(UploadError::failed("Drive upload source is empty"));
        }
        ensure_not_cancelled(&spec.cancel_file)?;
        let expected_md5 = file_md5(&spec.path, &spec.cancel_file).await?;
        ensure_not_cancelled(&spec.cancel_file)?;
        let session = self
            .start_drive_session(&DriveSessionRequest {
                request_id: spec.request_id,
                candidates: spec.candidates.clone(),
                content_length: metadata.len(),
                content_type: spec.content_type,
                expected_md5: expected_md5.clone(),
            })
            .await
            .map_err(|error| UploadError::failed(error.to_string()))?;
        let selected = match spec.candidates.get(session.candidate_index) {
            Some(selected) => selected,
            None => {
                self.cleanup_drive_session(&session).await;
                return Err(UploadError::failed(
                    "Lumiere selected an unknown Drive profile",
                ));
            }
        };
        if selected.profile != session.profile
            || selected.root != session.root
            || !valid_drive_id(&session.file_id)
            || !valid_drive_id(&session.parent_id)
            || !valid_delete_token(&session.delete_token)
        {
            self.cleanup_drive_session(&session).await;
            return Err(UploadError::failed(
                "Lumiere Drive profile response did not match the request",
            ));
        }
        let session_url = match validate_drive_session_url(&session.upload_url) {
            Ok(url) => url,
            Err(error) => {
                self.cleanup_drive_session(&session).await;
                return Err(error);
            }
        };
        let final_file = match upload_drive_chunks(
            &spec.path,
            metadata.len(),
            &session_url,
            &spec.cancel_file,
            progress,
        )
        .await
        {
            Ok(file) => file,
            Err(error) => {
                self.cleanup_drive_session(&session).await;
                return Err(error);
            }
        };

        let checksum_matches = final_file
            .md5_checksum
            .as_deref()
            .map(|checksum| checksum.eq_ignore_ascii_case(&expected_md5))
            .unwrap_or(false);
        let size_matches = final_file
            .size
            .as_deref()
            .and_then(|size| size.parse::<u64>().ok())
            == Some(metadata.len());
        let id_matches = final_file.id == session.file_id;
        if !checksum_matches || !size_matches || !id_matches {
            let removed = self
                .delete_drive_file(
                    session.profile.clone(),
                    session.file_id.clone(),
                    session.delete_token.clone(),
                )
                .await
                .is_ok();
            let message = if removed {
                "Google Drive verification failed; the unverified file was removed"
            } else {
                "Google Drive verification failed; broker cleanup also failed"
            };
            return Err(UploadError::failed(message));
        }

        Ok(DriveUploadResult {
            url: format!(
                "https://drive.google.com/file/d/{}/view?usp=sharing",
                final_file.id
            ),
            file_id: final_file.id,
            parent_id: session.parent_id,
            profile: session.profile,
            root: session.root,
            candidate_index: session.candidate_index,
            delete_token: session.delete_token,
        })
    }

    async fn cleanup_drive_session(&self, session: &DriveSessionResponse) {
        self.delete_drive_file(
            session.profile.clone(),
            session.file_id.clone(),
            session.delete_token.clone(),
        )
        .await
        .ok();
    }

    pub async fn upload_remote(
        &self,
        spec: RemoteUploadSpec,
        progress: Option<UnboundedSender<UploadProgress>>,
    ) -> Result<RemoteUploadResult, UploadError> {
        if spec.request_id.trim().is_empty() || spec.filename.trim().is_empty() {
            return Err(UploadError::failed("remote upload request is incomplete"));
        }
        ensure_not_cancelled(&spec.cancel_file)?;
        let lease = register_transfer(
            &spec.path,
            &spec.filename,
            &spec.content_type,
            self.config(),
        )
        .await
        .map_err(|error| UploadError::failed(error.to_string()))?;
        let operation = self
            .start_remote(&RemoteStartRequest {
                request_id: spec.request_id,
                provider: spec.provider,
                source_url: lease.url().to_string(),
                filename: spec.filename,
            })
            .await
            .map_err(|error| UploadError::failed(error.to_string()))?;
        if operation.provider != spec.provider
            || !valid_remote_id(&operation.file_code)
            || !valid_remote_id(&operation.operation_id)
        {
            return Err(UploadError::failed(
                "Lumiere returned an invalid remote upload operation",
            ));
        }

        let deadline = Instant::now() + self.config().transfer_ttl();
        let mut status_errors = 0u8;
        loop {
            ensure_not_cancelled(&spec.cancel_file)?;
            if Instant::now() >= deadline {
                return Err(UploadError::failed("remote upload capability expired"));
            }
            match self.remote_status(operation.clone()).await {
                Ok(status) => {
                    status_errors = 0;
                    let served = remote_progress_bytes(
                        lease.bytes_served(),
                        lease.size(),
                        status.bytes_done,
                        status.bytes_total,
                        status.progress,
                    );
                    send_progress(&progress, served, lease.size());
                    match status.state {
                        RemoteState::Complete => {
                            let expected_url = spec.provider.final_url(operation.file_code.trim());
                            let url = status
                                .url
                                .filter(|url| url == &expected_url)
                                .unwrap_or(expected_url);
                            return Ok(RemoteUploadResult {
                                url,
                                file_code: operation.file_code,
                            });
                        }
                        RemoteState::Failed => {
                            return Err(UploadError::failed(format!(
                                "{} remote upload failed",
                                spec.provider.label()
                            )));
                        }
                        RemoteState::Queued | RemoteState::Uploading => {}
                    }
                }
                Err(_) => {
                    status_errors = status_errors.saturating_add(1);
                    if status_errors >= REMOTE_STATUS_ERROR_LIMIT {
                        return Err(UploadError::failed(format!(
                            "{} remote upload status is unavailable",
                            spec.provider.label()
                        )));
                    }
                }
            }
            tokio::time::sleep(self.config().poll_interval()).await;
        }
    }
}

pub fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp4") | Some("m4v") | Some("mov") => "video/mp4",
        Some("mkv") => "video/x-matroska",
        Some("webm") => "video/webm",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

fn validate_drive_spec(spec: &DriveUploadSpec) -> Result<(), UploadError> {
    if spec.request_id.trim().is_empty() || spec.candidates.is_empty() {
        return Err(UploadError::failed("Drive upload request is incomplete"));
    }
    if spec.candidates.len() > 8 {
        return Err(UploadError::failed(
            "Drive upload has too many profile candidates",
        ));
    }
    Ok(())
}

async fn file_md5(path: &Path, cancel_file: &Option<PathBuf>) -> Result<String, UploadError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| UploadError::failed("upload source is unavailable"))?;
    let mut context = Md5Context::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        ensure_not_cancelled(cancel_file)?;
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|_| UploadError::failed("failed to verify upload source"))?;
        if read == 0 {
            break;
        }
        context.consume(&buffer[..read]);
    }
    Ok(format!("{:x}", context.compute()))
}

async fn upload_drive_chunks(
    path: &Path,
    total: u64,
    session_url: &Url,
    cancel_file: &Option<PathBuf>,
    progress: Option<UnboundedSender<UploadProgress>>,
) -> Result<DriveFileResponse, UploadError> {
    let http = HttpClient::builder()
        .connect_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(15 * 60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| UploadError::failed("failed to create Drive upload client"))?;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| UploadError::failed("Drive upload source is unavailable"))?;
    let mut offset = 0u64;
    let mut failures = 0u8;

    while offset < total {
        ensure_not_cancelled(cancel_file)?;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|_| UploadError::failed("failed to seek Drive upload source"))?;
        let chunk_len = (total - offset).min(DRIVE_CHUNK_SIZE as u64) as usize;
        let mut chunk = vec![0u8; chunk_len];
        file.read_exact(&mut chunk)
            .await
            .map_err(|_| UploadError::failed("failed to read Drive upload source"))?;
        let end = offset + chunk_len as u64 - 1;
        let response = http
            .put(session_url.clone())
            .header(header::CONTENT_LENGTH, chunk_len)
            .header(
                header::CONTENT_RANGE,
                format!("bytes {offset}-{end}/{total}"),
            )
            .body(chunk)
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                send_progress(&progress, total, total);
                return decode_drive_file(response).await;
            }
            Ok(response) if response.status() == StatusCode::PERMANENT_REDIRECT => {
                let acknowledged = acknowledged_offset(response.headers(), total)?;
                if acknowledged <= offset {
                    failures = failures.saturating_add(1);
                    if failures > DRIVE_RETRY_LIMIT {
                        return Err(UploadError::failed("Google Drive upload stopped advancing"));
                    }
                    tokio::time::sleep(retry_delay(failures)).await;
                } else {
                    failures = 0;
                }
                offset = acknowledged;
                send_progress(&progress, offset, total);
            }
            Ok(response) if response.status().is_server_error() => {
                failures = failures.saturating_add(1);
                if failures > DRIVE_RETRY_LIMIT {
                    return Err(UploadError::failed(
                        "Google Drive upload retry limit reached",
                    ));
                }
                tokio::time::sleep(retry_delay(failures)).await;
                match query_drive_offset(&http, session_url, total).await? {
                    DrivePosition::Offset(position) => {
                        offset = position;
                        send_progress(&progress, offset, total);
                    }
                    DrivePosition::Complete(file) => return Ok(file),
                }
            }
            Ok(_) => return Err(UploadError::failed("Google Drive rejected an upload chunk")),
            Err(_) => {
                failures = failures.saturating_add(1);
                if failures > DRIVE_RETRY_LIMIT {
                    return Err(UploadError::failed("Google Drive upload connection failed"));
                }
                tokio::time::sleep(retry_delay(failures)).await;
                match query_drive_offset(&http, session_url, total).await? {
                    DrivePosition::Offset(position) => {
                        offset = position;
                        send_progress(&progress, offset, total);
                    }
                    DrivePosition::Complete(file) => return Ok(file),
                }
            }
        }
    }
    Err(UploadError::failed(
        "Google Drive upload ended without file metadata",
    ))
}

enum DrivePosition {
    Offset(u64),
    Complete(DriveFileResponse),
}

async fn query_drive_offset(
    http: &HttpClient,
    session_url: &Url,
    total: u64,
) -> Result<DrivePosition, UploadError> {
    let response = http
        .put(session_url.clone())
        .header(header::CONTENT_LENGTH, 0)
        .header(header::CONTENT_RANGE, format!("bytes */{total}"))
        .send()
        .await
        .map_err(|_| UploadError::failed("Google Drive upload status request failed"))?;
    if response.status().is_success() {
        return decode_drive_file(response)
            .await
            .map(DrivePosition::Complete);
    }
    if response.status() == StatusCode::PERMANENT_REDIRECT {
        return acknowledged_offset(response.headers(), total).map(DrivePosition::Offset);
    }
    Err(UploadError::failed(
        "Google Drive upload session is no longer available",
    ))
}

fn acknowledged_offset(headers: &header::HeaderMap, total: u64) -> Result<u64, UploadError> {
    let Some(raw) = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(0);
    };
    let range = raw
        .trim()
        .strip_prefix("bytes=")
        .ok_or_else(|| UploadError::failed("Google Drive returned an invalid upload range"))?;
    let (_, end) = range
        .split_once('-')
        .ok_or_else(|| UploadError::failed("Google Drive returned an invalid upload range"))?;
    let end = end
        .parse::<u64>()
        .map_err(|_| UploadError::failed("Google Drive returned an invalid upload range"))?;
    Ok(end.saturating_add(1).min(total))
}

async fn decode_drive_file(response: reqwest::Response) -> Result<DriveFileResponse, UploadError> {
    response
        .json::<DriveFileResponse>()
        .await
        .map_err(|_| UploadError::failed("Google Drive returned invalid file metadata"))
}

fn valid_remote_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_drive_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_delete_token(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_drive_session_url(raw: &str) -> Result<Url, UploadError> {
    let url = Url::parse(raw)
        .map_err(|_| UploadError::failed("Lumiere returned an invalid Drive upload URL"))?;
    let valid_host = url.scheme() == "https"
        && url.host_str() == Some("www.googleapis.com")
        && url.port_or_known_default() == Some(443);
    let valid_path = url.path() == "/upload/drive/v3/files";
    let mut resumable = false;
    let mut upload_id = false;
    for (key, value) in url.query_pairs() {
        if key == "uploadType" && value == "resumable" {
            resumable = true;
        }
        if key == "upload_id" && !value.is_empty() {
            upload_id = true;
        }
    }
    if !valid_host
        || !valid_path
        || !resumable
        || !upload_id
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(UploadError::failed(
            "Lumiere returned an untrusted Drive upload URL",
        ));
    }
    Ok(url)
}

fn ensure_not_cancelled(cancel_file: &Option<PathBuf>) -> Result<(), UploadError> {
    if cancel_file
        .as_ref()
        .map(|path| path.exists())
        .unwrap_or(false)
    {
        Err(UploadError::Cancelled)
    } else {
        Ok(())
    }
}

fn send_progress(progress: &Option<UnboundedSender<UploadProgress>>, sent: u64, total: u64) {
    if let Some(progress) = progress {
        progress
            .send(UploadProgress {
                sent: sent.min(total),
                total,
            })
            .ok();
    }
}

fn remote_progress_bytes(
    served: u64,
    total: u64,
    provider_done: Option<u64>,
    provider_total: Option<u64>,
    provider_percent: Option<f64>,
) -> u64 {
    let provider_bytes = match (provider_done, provider_total) {
        (Some(done), Some(provider_total)) if provider_total > 0 => {
            ((done as f64 / provider_total as f64) * total as f64) as u64
        }
        _ => provider_percent
            .filter(|percent| percent.is_finite())
            .map(|percent| ((percent.clamp(0.0, 100.0) / 100.0) * total as f64) as u64)
            .unwrap_or(0),
    };
    served.max(provider_bytes).min(total)
}

fn retry_delay(failures: u8) -> Duration {
    Duration::from_secs((1u64 << failures.min(5)).min(30))
}

#[derive(Debug, Deserialize)]
struct DriveFileResponse {
    id: String,
    #[serde(rename = "md5Checksum")]
    md5_checksum: Option<String>,
    size: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    Cancelled,
    Failed(String),
}

impl UploadError {
    fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl Display for UploadError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(formatter, "upload cancelled"),
            Self::Failed(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for UploadError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_session_urls_are_host_and_path_pinned() {
        assert!(validate_drive_session_url(
            "https://www.googleapis.com/upload/drive/v3/files?uploadType=resumable&upload_id=abc"
        )
        .is_ok());
        assert!(
            validate_drive_session_url(
                "https://evil.example/upload/drive/v3/files?uploadType=resumable&upload_id=abc"
            )
            .is_err()
        );
        assert!(
            validate_drive_session_url(
                "https://www.googleapis.com:444/upload/drive/v3/files?uploadType=resumable&upload_id=abc"
            )
            .is_err()
        );
    }

    #[test]
    fn remote_progress_uses_the_best_available_signal() {
        assert_eq!(
            remote_progress_bytes(20, 100, Some(50), Some(100), None),
            50
        );
        assert_eq!(
            remote_progress_bytes(80, 100, Some(50), Some(100), None),
            80
        );
        assert_eq!(remote_progress_bytes(0, 100, None, None, Some(25.0)), 25);
    }

    #[test]
    fn content_types_are_conservative() {
        assert_eq!(content_type_for_path(Path::new("release.mp4")), "video/mp4");
        assert_eq!(
            content_type_for_path(Path::new("fonts.zip")),
            "application/zip"
        );
        assert_eq!(
            content_type_for_path(Path::new("unknown.bin")),
            "application/octet-stream"
        );
    }
}
