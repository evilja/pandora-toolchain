use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{
    LUMIERE_BROKER_TOKEN, LUMIERE_BROKER_URL, LUMIERE_POLL_INTERVAL_SECS, LUMIERE_PUBLIC_URL,
    LUMIERE_REMOTE_STALL_SECS, LUMIERE_TRANSFER_TTL_SECS,
};
use reqwest::{Client as HttpClient, StatusCode, Url};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt::{Display, Formatter};
use std::time::Duration;

use super::observe::{duration_label, info, trace, transport_reason, warn};
use super::protocol::{
    API_VERSION, DriveDeleteRequest, DriveSessionRequest, DriveSessionResponse, ErrorEnvelope,
    ProviderStatus, RemoteOperation, RemoteStartRequest, RemoteStatusRequest, RemoteStatusResponse,
    StatusResponse,
};

const BROKER_SCOPE: &str = "broker";

const DEFAULT_TRANSFER_TTL_SECS: u64 = 6 * 60 * 60;
const MIN_TRANSFER_TTL_SECS: u64 = 5 * 60;
const MAX_TRANSFER_TTL_SECS: u64 = 24 * 60 * 60;
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
const DEFAULT_REMOTE_STALL_SECS: u64 = 15 * 60;
const MIN_REMOTE_STALL_SECS: u64 = 2 * 60;
const MAX_REMOTE_STALL_SECS: u64 = 6 * 60 * 60;

#[derive(Clone)]
pub struct Config {
    broker_url: Url,
    broker_token: String,
    public_url: Url,
    transfer_ttl: Duration,
    poll_interval: Duration,
    remote_stall: Option<Duration>,
}

impl Config {
    pub fn from_env() -> Result<Self, Error> {
        let env = get_pandora_env();
        let broker_url = required(&env, LUMIERE_BROKER_URL)?;
        let broker_token = required(&env, LUMIERE_BROKER_TOKEN)?;
        let public_url = required(&env, LUMIERE_PUBLIC_URL)?;
        let transfer_ttl = env
            .get(LUMIERE_TRANSFER_TTL_SECS)
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_TRANSFER_TTL_SECS)
            .clamp(MIN_TRANSFER_TTL_SECS, MAX_TRANSFER_TTL_SECS);
        let poll_interval = env
            .get(LUMIERE_POLL_INTERVAL_SECS)
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECS)
            .clamp(2, 60);
        // `0` disables the guard; anything else is clamped into a range that
        // cannot fail a provider that is merely queued behind other jobs.
        let remote_stall = match env
            .get(LUMIERE_REMOTE_STALL_SECS)
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_REMOTE_STALL_SECS)
        {
            0 => None,
            seconds => Some(Duration::from_secs(
                seconds.clamp(MIN_REMOTE_STALL_SECS, MAX_REMOTE_STALL_SECS),
            )),
        };
        Self::new(
            &broker_url,
            broker_token,
            &public_url,
            Duration::from_secs(transfer_ttl),
            Duration::from_secs(poll_interval),
            remote_stall,
        )
    }

    fn new(
        broker_url: &str,
        broker_token: String,
        public_url: &str,
        transfer_ttl: Duration,
        poll_interval: Duration,
        remote_stall: Option<Duration>,
    ) -> Result<Self, Error> {
        let broker_url = parse_base_url(broker_url, LUMIERE_BROKER_URL)?;
        let public_url = parse_base_url(public_url, LUMIERE_PUBLIC_URL)?;
        if public_url.path() != "/" {
            return Err(Error::configuration(format!(
                "`{LUMIERE_PUBLIC_URL}` must contain only an origin"
            )));
        }
        if broker_token.trim().is_empty() {
            return Err(Error::configuration(format!(
                "`{LUMIERE_BROKER_TOKEN}` is empty"
            )));
        }
        Ok(Self {
            broker_url,
            broker_token,
            public_url,
            transfer_ttl,
            poll_interval,
            remote_stall,
        })
    }

    pub fn public_url(&self) -> &Url {
        &self.public_url
    }

    pub fn transfer_ttl(&self) -> Duration {
        self.transfer_ttl
    }

    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    pub fn remote_stall(&self) -> Option<Duration> {
        self.remote_stall
    }
}

#[derive(Clone)]
pub struct LumiereClient {
    config: Config,
    http: HttpClient,
}

impl LumiereClient {
    pub fn from_env() -> Result<Self, Error> {
        Self::new(Config::from_env()?)
    }

    pub fn new(config: Config) -> Result<Self, Error> {
        let http = HttpClient::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::configuration("failed to create the Lumiere HTTP client"))?;
        Ok(Self { config, http })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub async fn provider_status(
        &self,
        requested_profile: Option<&str>,
    ) -> Result<ProviderStatus, Error> {
        let mut url = self.endpoint(&["v1", "status"])?;
        if let Some(profile) = requested_profile
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            url.query_pairs_mut().append_pair("profile", profile);
        }
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.config.broker_token)
            .header("X-Lumiere-Version", API_VERSION)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|error| {
                warn(
                    BROKER_SCOPE,
                    format!("status request failed: {}", transport_reason(&error)),
                );
                Error::unavailable("Lumiere broker status request failed")
            })?;
        self.decode::<StatusResponse>(response, "v1/status")
            .await
            .map(|status| status.providers)
            .inspect(|providers| {
                info(
                    BROKER_SCOPE,
                    format!(
                        "provider status: global drive={} requested drive={} doodstream={} lulustream={} voe={} abyss={}",
                        providers.global_drive,
                        providers.requested_drive,
                        providers.doodstream,
                        providers.lulustream,
                        providers.voe,
                        providers.abyss
                    ),
                );
            })
    }

    pub(crate) async fn start_drive_session(
        &self,
        request: &DriveSessionRequest,
    ) -> Result<DriveSessionResponse, Error> {
        self.post(&["v1", "drive", "sessions"], request).await
    }

    pub async fn delete_drive_file(
        &self,
        profile: String,
        file_id: String,
        delete_token: String,
    ) -> Result<(), Error> {
        let request = DriveDeleteRequest {
            profile,
            file_id,
            delete_token,
        };
        let _: serde_json::Value = self.post(&["v1", "drive", "delete"], &request).await?;
        Ok(())
    }

    pub(crate) async fn start_remote(
        &self,
        request: &RemoteStartRequest,
    ) -> Result<RemoteOperation, Error> {
        self.post(&["v1", "remote", "start"], request).await
    }

    pub(crate) async fn remote_status(
        &self,
        operation: RemoteOperation,
        source_drained: bool,
    ) -> Result<RemoteStatusResponse, Error> {
        self.post(
            &["v1", "remote", "status"],
            &RemoteStatusRequest {
                operation,
                source_drained,
            },
        )
        .await
    }

    async fn post<T: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        segments: &[&str],
        body: &T,
    ) -> Result<R, Error> {
        let url = self.endpoint(segments)?;
        let route = segments.join("/");
        let started = std::time::Instant::now();
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.config.broker_token)
            .header("X-Lumiere-Version", API_VERSION)
            .json(body)
            .send()
            .await
            .map_err(|error| {
                warn(
                    BROKER_SCOPE,
                    format!(
                        "POST {route} never completed after {}: {}",
                        duration_label(started.elapsed()),
                        transport_reason(&error)
                    ),
                );
                Error::unavailable("Lumiere broker request failed")
            })?;
        trace(
            BROKER_SCOPE,
            format!(
                "POST {route} -> HTTP {} in {}",
                response.status().as_u16(),
                duration_label(started.elapsed())
            ),
        );
        self.decode(response, &route).await
    }

    async fn decode<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
        route: &str,
    ) -> Result<T, Error> {
        let status = response.status();
        if status.is_success() {
            return response.json::<T>().await.map_err(|error| {
                warn(
                    BROKER_SCOPE,
                    format!("{route} returned a body we could not parse: {error}"),
                );
                Error::protocol("Lumiere broker returned invalid JSON")
            });
        }
        let safe = response.json::<ErrorEnvelope>().await.ok();
        if let Some(safe) = safe {
            let message = truncate_message(&safe.error.message);
            warn(
                BROKER_SCOPE,
                format!(
                    "{route} rejected: HTTP {} {} — {message}",
                    status.as_u16(),
                    safe.error.code
                ),
            );
            return Err(Error::broker(status, safe.error.code, message));
        }
        warn(
            BROKER_SCOPE,
            format!("{route} returned bare HTTP {}", status.as_u16()),
        );
        Err(Error::broker(
            status,
            "http_error".to_string(),
            format!("Lumiere broker returned HTTP {}", status.as_u16()),
        ))
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url, Error> {
        let mut url = self.config.broker_url.clone();
        {
            let mut path = url.path_segments_mut().map_err(|_| {
                Error::configuration("Lumiere broker URL cannot contain a base path")
            })?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Configuration,
    Unavailable,
    Protocol,
    Broker,
}

#[derive(Clone, Debug)]
pub struct Error {
    kind: ErrorKind,
    code: String,
    message: String,
    status: Option<StatusCode>,
}

impl Error {
    fn configuration(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Configuration, "configuration", message, None)
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unavailable, "unavailable", message, None)
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Protocol, "protocol", message, None)
    }

    fn broker(status: StatusCode, code: String, message: String) -> Self {
        Self::new(ErrorKind::Broker, code, message, Some(status))
    }

    fn new(
        kind: ErrorKind,
        code: impl Into<String>,
        message: impl Into<String>,
        status: Option<StatusCode>,
    ) -> Self {
        Self {
            kind,
            code: code.into(),
            message: message.into(),
            status,
        }
    }

    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn status(&self) -> Option<StatusCode> {
        self.status
    }
}

impl Display for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for Error {}

fn required(env: &std::collections::HashMap<String, String>, key: &str) -> Result<String, Error> {
    env.get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::configuration(format!("`{key}` is not configured")))
}

fn parse_base_url(raw: &str, key: &str) -> Result<Url, Error> {
    let mut normalized = raw.trim().trim_end_matches('/').to_string();
    normalized.push('/');
    let url = Url::parse(&normalized)
        .map_err(|_| Error::configuration(format!("`{key}` is not a valid URL")))?;
    let local_http =
        url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"));
    if url.scheme() != "https" && !local_http {
        return Err(Error::configuration(format!("`{key}` must use HTTPS")));
    }
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::configuration(format!(
            "`{key}` must be a plain base URL"
        )));
    }
    Ok(url)
}

fn truncate_message(message: &str) -> String {
    let mut chars = message.trim().chars();
    let mut output = chars.by_ref().take(300).collect::<String>();
    if chars.next().is_some() {
        output.push('…');
    }
    if output.is_empty() {
        "Lumiere broker rejected the request".to_string()
    } else {
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_urls_require_https() {
        let error = Config::new(
            "http://broker.example",
            "token".to_string(),
            "https://files.example",
            Duration::from_secs(60),
            Duration::from_secs(5),
            Some(Duration::from_secs(900)),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), &ErrorKind::Configuration);
    }

    #[test]
    fn localhost_http_is_available_for_tests() {
        assert!(
            Config::new(
                "http://127.0.0.1:8789",
                "token".to_string(),
                "http://localhost:8787",
                Duration::from_secs(60),
                Duration::from_secs(5),
                None,
            )
            .is_ok()
        );
    }
}
