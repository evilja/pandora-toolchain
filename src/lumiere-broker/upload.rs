use md5::Context as Md5Context;
use reqwest::{Client as HttpClient, StatusCode, Url, header};
use serde::Deserialize;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::mpsc::UnboundedSender;

use super::client::LumiereClient;
use super::observe::{
    bytes_label, duration_label, info, rate_label, redact_url, response_body_excerpt, trace,
    transport_reason, warn,
};
use super::protocol::{
    DriveCandidate, DriveSessionRequest, DriveSessionResponse, RemoteProvider, RemoteStartRequest,
    RemoteState, RemoteStatusResponse,
};
use super::transfer::register_transfer;

const DRIVE_CHUNK_SIZE: usize = 8 * 1024 * 1024;
const DRIVE_RETRY_LIMIT: u8 = 5;
const REMOTE_STATUS_ERROR_LIMIT: u8 = 5;
const REMOTE_HEARTBEAT: Duration = Duration::from_secs(60);
// A provider that has not touched the capability URL by this point is not simply
// slow: it usually cannot reach it at all (Access login, bot challenge, wrong
// hostname), which is the failure this warning exists to name.
const REMOTE_FETCH_GRACE: Duration = Duration::from_secs(120);

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
        let scope = spec.request_id.clone();
        let started = Instant::now();
        validate_drive_spec(&spec).inspect_err(|error| warn(&scope, format!("rejected: {error}")))?;
        let metadata = tokio::fs::metadata(&spec.path).await.map_err(|error| {
            warn(
                &scope,
                format!("source {} is unreadable: {error}", spec.path.display()),
            );
            UploadError::failed("Drive upload source is unavailable")
        })?;
        if !metadata.is_file() || metadata.len() == 0 {
            warn(
                &scope,
                format!(
                    "source {} is not a non-empty file (len={})",
                    spec.path.display(),
                    metadata.len()
                ),
            );
            return Err(UploadError::failed("Drive upload source is empty"));
        }
        info(
            &scope,
            format!(
                "drive upload starting: {} ({}), candidates=[{}]",
                spec.path.display(),
                bytes_label(metadata.len()),
                describe_candidates(&spec.candidates)
            ),
        );
        ensure_not_cancelled(&spec.cancel_file)?;
        let hashing = Instant::now();
        let expected_md5 = file_md5(&spec.path, &spec.cancel_file).await?;
        trace(
            &scope,
            format!(
                "md5 {} computed in {}",
                expected_md5,
                duration_label(hashing.elapsed())
            ),
        );
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
            .map_err(|error| {
                warn(
                    &scope,
                    format!(
                        "broker refused the Drive session ({}{}): {error}",
                        error.code(),
                        error
                            .status()
                            .map(|status| format!(" http {}", status.as_u16()))
                            .unwrap_or_default()
                    ),
                );
                UploadError::failed(error.to_string())
            })?;
        info(
            &scope,
            format!(
                "drive session granted: profile={} root={} candidate={} file_id={}",
                session.profile, session.root, session.candidate_index, session.file_id
            ),
        );
        trace(
            &scope,
            format!("drive session url: {}", redact_url(&session.upload_url)),
        );
        let selected = match spec.candidates.get(session.candidate_index) {
            Some(selected) => selected,
            None => {
                warn(
                    &scope,
                    format!(
                        "broker chose candidate {} but only {} were offered",
                        session.candidate_index,
                        spec.candidates.len()
                    ),
                );
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
            warn(
                &scope,
                format!(
                    "session mismatch: asked {}/{}, got {}/{} (file_id ok={}, parent_id ok={}, delete token ok={})",
                    selected.profile,
                    selected.root,
                    session.profile,
                    session.root,
                    valid_drive_id(&session.file_id),
                    valid_drive_id(&session.parent_id),
                    valid_delete_token(&session.delete_token)
                ),
            );
            self.cleanup_drive_session(&session).await;
            return Err(UploadError::failed(
                "Lumiere Drive profile response did not match the request",
            ));
        }
        let session_url = match validate_drive_session_url(&session.upload_url) {
            Ok(url) => url,
            Err(error) => {
                warn(
                    &scope,
                    format!(
                        "untrusted Drive session url {}: {error}",
                        redact_url(&session.upload_url)
                    ),
                );
                self.cleanup_drive_session(&session).await;
                return Err(error);
            }
        };
        let final_file = match upload_drive_chunks(
            &scope,
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
                if error.is_cancelled() {
                    info(&scope, "drive upload cancelled");
                } else {
                    warn(&scope, format!("drive upload failed: {error}"));
                }
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
            warn(
                &scope,
                format!(
                    "drive verification failed: md5 expected {} got {}, size expected {} got {}, file_id expected {} got {}",
                    expected_md5,
                    final_file.md5_checksum.as_deref().unwrap_or("<none>"),
                    metadata.len(),
                    final_file.size.as_deref().unwrap_or("<none>"),
                    session.file_id,
                    final_file.id
                ),
            );
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

        info(
            &scope,
            format!(
                "drive upload complete in {} ({} avg): profile={} root={} file_id={}",
                duration_label(started.elapsed()),
                rate_label(metadata.len(), started.elapsed()),
                session.profile,
                session.root,
                final_file.id
            ),
        );
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
        match self
            .delete_drive_file(
                session.profile.clone(),
                session.file_id.clone(),
                session.delete_token.clone(),
            )
            .await
        {
            Ok(()) => info(
                "broker",
                format!("removed abandoned Drive file {}", session.file_id),
            ),
            Err(error) => warn(
                "broker",
                format!(
                    "abandoned Drive file {} could not be removed and needs manual cleanup: {error}",
                    session.file_id
                ),
            ),
        }
    }

    pub async fn upload_remote(
        &self,
        spec: RemoteUploadSpec,
        progress: Option<UnboundedSender<UploadProgress>>,
    ) -> Result<RemoteUploadResult, UploadError> {
        let scope = spec.request_id.clone();
        let provider = spec.provider.label();
        let started = Instant::now();
        if spec.request_id.trim().is_empty() || spec.filename.trim().is_empty() {
            warn(&scope, "remote upload request is incomplete");
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
        .map_err(|error| {
            warn(
                &scope,
                format!(
                    "could not publish {} for {provider}: {error}",
                    spec.path.display()
                ),
            );
            UploadError::failed(error.to_string())
        })?;
        info(
            &scope,
            format!(
                "{provider} remote upload starting: {} ({}), capability {} valid for {}",
                spec.filename,
                bytes_label(lease.size()),
                redact_url(lease.url()),
                duration_label(self.config().transfer_ttl())
            ),
        );
        let operation = self
            .start_remote(&RemoteStartRequest {
                request_id: spec.request_id,
                provider: spec.provider,
                source_url: lease.url().to_string(),
                filename: spec.filename,
            })
            .await
            .map_err(|error| {
                warn(
                    &scope,
                    format!(
                        "broker refused to start the {provider} upload ({}{}): {error}",
                        error.code(),
                        error
                            .status()
                            .map(|status| format!(" http {}", status.as_u16()))
                            .unwrap_or_default()
                    ),
                );
                UploadError::failed(error.to_string())
            })?;
        if operation.provider != spec.provider
            || !valid_remote_id(&operation.file_code)
            || !valid_remote_id(&operation.operation_id)
        {
            warn(
                &scope,
                format!(
                    "broker returned an unusable {provider} operation: provider={:?} operation_id={:?} file_code={:?}",
                    operation.provider, operation.operation_id, operation.file_code
                ),
            );
            return Err(UploadError::failed(
                "Lumiere returned an invalid remote upload operation",
            ));
        }
        info(
            &scope,
            format!(
                "{provider} accepted the job: operation={} file_code={}",
                operation.operation_id, operation.file_code
            ),
        );

        let deadline = Instant::now() + self.config().transfer_ttl();
        let stall_limit = self.config().remote_stall();
        let mut status_errors = 0u8;
        let mut last_state: Option<RemoteState> = None;
        let mut next_heartbeat = Instant::now() + REMOTE_HEARTBEAT;
        let mut fetch_warned = false;
        let mut last_movement = Instant::now();
        let mut movement_mark = RemoteMovement::default();
        loop {
            ensure_not_cancelled(&spec.cancel_file).inspect_err(|_| {
                info(
                    &scope,
                    format!(
                        "{provider} upload cancelled after {} (served {})",
                        duration_label(started.elapsed()),
                        bytes_label(lease.bytes_served())
                    ),
                );
            })?;
            if Instant::now() >= deadline {
                warn(
                    &scope,
                    format!(
                        "{provider} upload expired after {} in state {}: the provider served {} of {} — the host never finished pulling the file",
                        duration_label(started.elapsed()),
                        state_label(last_state),
                        bytes_label(lease.bytes_served()),
                        bytes_label(lease.size())
                    ),
                );
                return Err(UploadError::failed("remote upload capability expired"));
            }
            let source_drained = lease.bytes_served() >= lease.size();
            match self.remote_status(operation.clone(), source_drained).await {
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
                    let movement = RemoteMovement::new(&status, lease.bytes_served());
                    if movement != movement_mark {
                        movement_mark = movement;
                        last_movement = Instant::now();
                    }
                    if last_state != Some(status.state) {
                        info(
                            &scope,
                            format!(
                                "{provider} state {} -> {:?} after {} ({})",
                                state_label(last_state),
                                status.state,
                                duration_label(started.elapsed()),
                                detail_label(&status.detail)
                            ),
                        );
                        last_state = Some(status.state);
                    } else if Instant::now() >= next_heartbeat {
                        next_heartbeat = Instant::now() + REMOTE_HEARTBEAT;
                        info(
                            &scope,
                            format!(
                                "{provider} still {:?} after {}: we served {} of {}, provider reports {} ({})",
                                status.state,
                                duration_label(started.elapsed()),
                                bytes_label(lease.bytes_served()),
                                bytes_label(lease.size()),
                                provider_progress_label(
                                    status.bytes_done,
                                    status.bytes_total,
                                    status.progress
                                ),
                                detail_label(&status.detail)
                            ),
                        );
                    } else {
                        trace(
                            &scope,
                            format!(
                                "{provider} poll: state={:?} served={} provider={} ({})",
                                status.state,
                                bytes_label(lease.bytes_served()),
                                provider_progress_label(
                                    status.bytes_done,
                                    status.bytes_total,
                                    status.progress
                                ),
                                detail_label(&status.detail)
                            ),
                        );
                    }
                    // Zero bytes served means the provider's fetcher never reached
                    // the capability URL, which is a Cloudflare/hostname problem on
                    // our side rather than a slow provider queue.
                    if !fetch_warned
                        && lease.bytes_served() == 0
                        && started.elapsed() >= REMOTE_FETCH_GRACE
                    {
                        fetch_warned = true;
                        warn(
                            &scope,
                            format!(
                                "{provider} has not requested {} in {} — check that the file hostname is reachable without Access login or a bot challenge",
                                redact_url(lease.url()),
                                duration_label(started.elapsed())
                            ),
                        );
                    }
                    match status.state {
                        RemoteState::Complete => {
                            let expected_url = spec
                                .provider
                                .final_url_on(status.embed_domain.as_deref(), operation.file_code.trim());
                            let url = status
                                .url
                                .filter(|url| url == &expected_url)
                                .unwrap_or(expected_url);
                            info(
                                &scope,
                                format!(
                                    "{provider} upload complete in {} ({} served, {} avg): {url}",
                                    duration_label(started.elapsed()),
                                    bytes_label(lease.bytes_served()),
                                    rate_label(lease.bytes_served(), started.elapsed())
                                ),
                            );
                            return Ok(RemoteUploadResult {
                                url,
                                file_code: operation.file_code,
                            });
                        }
                        RemoteState::Failed => {
                            warn(
                                &scope,
                                format!(
                                    "{provider} reported failure after {} (served {} of {}): {}",
                                    duration_label(started.elapsed()),
                                    bytes_label(lease.bytes_served()),
                                    bytes_label(lease.size()),
                                    detail_label(&status.detail)
                                ),
                            );
                            return Err(UploadError::failed(format!(
                                "{} remote upload failed",
                                spec.provider.label()
                            )));
                        }
                        // A host that stops moving would otherwise hold the job —
                        // and its Discord message — until the transfer TTL, so give
                        // up on it and let the remaining hosts finish the release.
                        RemoteState::Queued | RemoteState::Uploading => {
                            if let Some(limit) = stall_limit
                                && last_movement.elapsed() >= limit
                            {
                                warn(
                                    &scope,
                                    format!(
                                        "{provider} has not moved in {} (state {:?} after {}, served {} of {}, provider reports {}) — giving up on this host",
                                        duration_label(last_movement.elapsed()),
                                        status.state,
                                        duration_label(started.elapsed()),
                                        bytes_label(lease.bytes_served()),
                                        bytes_label(lease.size()),
                                        provider_progress_label(
                                            status.bytes_done,
                                            status.bytes_total,
                                            status.progress
                                        )
                                    ),
                                );
                                return Err(UploadError::failed(format!(
                                    "{} remote upload stalled",
                                    spec.provider.label()
                                )));
                            }
                        }
                    }
                }
                Err(error) => {
                    status_errors = status_errors.saturating_add(1);
                    warn(
                        &scope,
                        format!(
                            "{provider} status check {status_errors}/{REMOTE_STATUS_ERROR_LIMIT} failed ({}{}): {error}",
                            error.code(),
                            error
                                .status()
                                .map(|status| format!(" http {}", status.as_u16()))
                                .unwrap_or_default()
                        ),
                    );
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

// Forward progress for the stall guard: any change in what the provider tells us,
// or in how much of the file we have handed over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RemoteMovement {
    state: Option<RemoteState>,
    served: u64,
    provider_done: Option<u64>,
    provider_permille: Option<u64>,
}

impl RemoteMovement {
    fn new(status: &RemoteStatusResponse, served: u64) -> Self {
        Self {
            state: Some(status.state),
            served,
            provider_done: status.bytes_done,
            provider_permille: status
                .progress
                .filter(|percent| percent.is_finite())
                .map(|percent| (percent.clamp(0.0, 100.0) * 10.0) as u64),
        }
    }
}

fn detail_label(detail: &Option<String>) -> String {
    detail
        .as_deref()
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
        .unwrap_or("no detail from provider")
        .to_string()
}

fn state_label(state: Option<RemoteState>) -> String {
    state
        .map(|state| format!("{state:?}"))
        .unwrap_or_else(|| "<none>".to_string())
}

fn provider_progress_label(
    bytes_done: Option<u64>,
    bytes_total: Option<u64>,
    percent: Option<f64>,
) -> String {
    match (bytes_done, bytes_total) {
        (Some(done), Some(total)) => format!("{}/{}", bytes_label(done), bytes_label(total)),
        (Some(done), None) => bytes_label(done),
        _ => percent
            .filter(|percent| percent.is_finite())
            .map(|percent| format!("{percent:.1}%"))
            .unwrap_or_else(|| "no progress reported".to_string()),
    }
}

fn describe_candidates(candidates: &[DriveCandidate]) -> String {
    candidates
        .iter()
        .map(|candidate| {
            format!(
                "{}:{}:{}",
                candidate.profile, candidate.root, candidate.folder_path
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
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
    scope: &str,
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
    let mut chunk_index = 0u64;
    let started = Instant::now();

    while offset < total {
        ensure_not_cancelled(cancel_file)?;
        let chunk_started = Instant::now();
        chunk_index += 1;
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
                trace(
                    scope,
                    format!(
                        "chunk {chunk_index} finalised the upload ({} in {})",
                        bytes_label(chunk_len as u64),
                        duration_label(chunk_started.elapsed())
                    ),
                );
                send_progress(&progress, total, total);
                return decode_drive_file(response).await.inspect_err(|error| {
                    warn(scope, format!("final Drive metadata unusable: {error}"));
                });
            }
            Ok(response) if response.status() == StatusCode::PERMANENT_REDIRECT => {
                let acknowledged = acknowledged_offset(response.headers(), total)?;
                if acknowledged <= offset {
                    failures = failures.saturating_add(1);
                    warn(
                        scope,
                        format!(
                            "chunk {chunk_index} acknowledged {} of an expected {} — no progress (attempt {failures}/{DRIVE_RETRY_LIMIT})",
                            acknowledged,
                            offset + chunk_len as u64
                        ),
                    );
                    if failures > DRIVE_RETRY_LIMIT {
                        warn(
                            scope,
                            format!("giving up: Drive stopped advancing at {}", bytes_label(offset)),
                        );
                        return Err(UploadError::failed("Google Drive upload stopped advancing"));
                    }
                    tokio::time::sleep(retry_delay(failures)).await;
                } else {
                    failures = 0;
                    trace(
                        scope,
                        format!(
                            "chunk {chunk_index} ok: {}/{} ({} in {}, {} avg)",
                            bytes_label(acknowledged),
                            bytes_label(total),
                            bytes_label(acknowledged.saturating_sub(offset)),
                            duration_label(chunk_started.elapsed()),
                            rate_label(acknowledged, started.elapsed())
                        ),
                    );
                }
                offset = acknowledged;
                send_progress(&progress, offset, total);
            }
            Ok(response) if response.status().is_server_error() => {
                failures = failures.saturating_add(1);
                warn(
                    scope,
                    format!(
                        "Drive returned HTTP {} at offset {} (attempt {failures}/{DRIVE_RETRY_LIMIT})",
                        response.status().as_u16(),
                        offset
                    ),
                );
                if failures > DRIVE_RETRY_LIMIT {
                    return Err(UploadError::failed(
                        "Google Drive upload retry limit reached",
                    ));
                }
                tokio::time::sleep(retry_delay(failures)).await;
                match query_drive_offset(scope, &http, session_url, total).await? {
                    DrivePosition::Offset(position) => {
                        offset = position;
                        send_progress(&progress, offset, total);
                    }
                    DrivePosition::Complete(file) => return Ok(file),
                }
            }
            Ok(response) => {
                warn(
                    scope,
                    format!(
                        "Drive rejected chunk {chunk_index} at offset {offset} with HTTP {}: {}",
                        response.status().as_u16(),
                        response_body_excerpt(response).await
                    ),
                );
                return Err(UploadError::failed("Google Drive rejected an upload chunk"));
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                warn(
                    scope,
                    format!(
                        "Drive chunk {chunk_index} transport error at offset {offset} after {} (attempt {failures}/{DRIVE_RETRY_LIMIT}): {}",
                        duration_label(chunk_started.elapsed()),
                        transport_reason(&error)
                    ),
                );
                if failures > DRIVE_RETRY_LIMIT {
                    return Err(UploadError::failed("Google Drive upload connection failed"));
                }
                tokio::time::sleep(retry_delay(failures)).await;
                match query_drive_offset(scope, &http, session_url, total).await? {
                    DrivePosition::Offset(position) => {
                        offset = position;
                        send_progress(&progress, offset, total);
                    }
                    DrivePosition::Complete(file) => return Ok(file),
                }
            }
        }
    }
    warn(
        scope,
        format!(
            "Drive acknowledged all {} bytes but never returned file metadata",
            total
        ),
    );
    Err(UploadError::failed(
        "Google Drive upload ended without file metadata",
    ))
}


enum DrivePosition {
    Offset(u64),
    Complete(DriveFileResponse),
}

async fn query_drive_offset(
    scope: &str,
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
        .map_err(|error| {
            warn(
                scope,
                format!("Drive resume query failed: {}", transport_reason(&error)),
            );
            UploadError::failed("Google Drive upload status request failed")
        })?;
    if response.status().is_success() {
        return decode_drive_file(response)
            .await
            .map(DrivePosition::Complete);
    }
    if response.status() == StatusCode::PERMANENT_REDIRECT {
        let offset = acknowledged_offset(response.headers(), total)?;
        info(
            scope,
            format!("resuming Drive upload from {}", bytes_label(offset)),
        );
        return Ok(DrivePosition::Offset(offset));
    }
    warn(
        scope,
        format!(
            "Drive session is gone (HTTP {}): {}",
            response.status().as_u16(),
            response_body_excerpt(response).await
        ),
    );
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

    fn status(state: RemoteState, progress: Option<f64>, bytes_done: Option<u64>) -> RemoteStatusResponse {
        RemoteStatusResponse {
            state,
            embed_domain: None,
            progress,
            bytes_done,
            bytes_total: Some(100),
            url: None,
            detail: None,
        }
    }

    #[test]
    fn stall_guard_only_resets_on_real_movement() {
        let queued = RemoteMovement::new(&status(RemoteState::Queued, None, None), 0);
        assert_eq!(
            queued,
            RemoteMovement::new(&status(RemoteState::Queued, None, None), 0),
            "an identical poll must not count as progress"
        );
        assert_ne!(
            queued,
            RemoteMovement::new(&status(RemoteState::Uploading, None, None), 0),
            "a state change is progress"
        );
        assert_ne!(
            queued,
            RemoteMovement::new(&status(RemoteState::Queued, None, None), 4096),
            "bytes leaving our side is progress"
        );
        assert_ne!(
            RemoteMovement::new(&status(RemoteState::Uploading, Some(12.0), None), 100),
            RemoteMovement::new(&status(RemoteState::Uploading, Some(12.5), None), 100),
            "provider percentage movement is progress"
        );
    }

    #[test]
    fn missing_provider_detail_is_named_rather_than_blank() {
        assert_eq!(detail_label(&None), "no detail from provider");
        assert_eq!(detail_label(&Some("  ".to_string())), "no detail from provider");
        assert_eq!(
            detail_label(&Some("status=working".to_string())),
            "status=working"
        );
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
