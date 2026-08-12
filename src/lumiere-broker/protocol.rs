use serde::{Deserialize, Serialize};

pub const API_VERSION: &str = "1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DriveCandidate {
    pub profile: String,
    pub root: String,
    pub folder_path: String,
    pub filename: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct DriveSessionRequest {
    pub request_id: String,
    pub candidates: Vec<DriveCandidate>,
    pub content_length: u64,
    pub content_type: String,
    pub expected_md5: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DriveSessionResponse {
    pub upload_url: String,
    pub candidate_index: usize,
    pub profile: String,
    pub root: String,
    pub parent_id: String,
    pub file_id: String,
    pub delete_token: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct DriveDeleteRequest {
    pub profile: String,
    pub file_id: String,
    pub delete_token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteProvider {
    Lulustream,
    Voe,
    Byse,
}

impl RemoteProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Lulustream => "Lulustream",
            Self::Voe => "Voe",
            Self::Byse => "Byse",
        }
    }

    // Player origins rotate independently of the provider API origins, and a
    // published link has to name the live host itself because AnimeciX/OpenAnime/
    // Anizm store it and embed it long after the redirect chain moves again. This
    // is the fallback host; when the broker reports a current one, `final_url_on`
    // composes the link from that instead. Mirrors PROVIDER_EMBED in
    // cloudflare/lumiere-broker/src/index.js.
    pub fn final_url(self, file_code: &str) -> String {
        let host = match self {
            Self::Lulustream => "luluvdo.com",
            Self::Voe => "voe.sx",
            Self::Byse => "byse.sx",
        };
        format!("https://{host}/e/{file_code}")
    }

    // The broker reports the provider's current player domain as a bare hostname.
    // Pandora composes the URL itself rather than publishing whatever string the
    // provider handed back, so a compromised or confused provider can still only
    // move the link to a host it names — never add a path, port, or credentials.
    pub fn final_url_on(self, domain: Option<&str>, file_code: &str) -> String {
        match domain.and_then(valid_embed_domain) {
            Some(host) => format!("https://{host}/e/{file_code}"),
            None => self.final_url(file_code),
        }
    }
}

fn valid_embed_domain(raw: &str) -> Option<&str> {
    let value = raw.trim();
    if value.is_empty() || value.len() > 100 {
        return None;
    }
    let labelled = value.split('.').collect::<Vec<_>>();
    if labelled.len() < 2 {
        return None;
    }
    let usable = labelled.iter().all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    });
    usable.then_some(value)
}

#[derive(Debug, Serialize)]
pub(crate) struct RemoteStartRequest {
    pub request_id: String,
    pub provider: RemoteProvider,
    pub source_url: String,
    pub filename: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RemoteOperation {
    pub provider: RemoteProvider,
    pub operation_id: String,
    pub file_code: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct RemoteStatusRequest {
    pub operation: RemoteOperation,
    // True once the provider has pulled every byte from the capability URL. The
    // Worker uses it to confirm completion through the provider's file record,
    // because some providers stop reporting a queue entry they have finished.
    pub source_drained: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemoteState {
    Queued,
    Uploading,
    Complete,
    Failed,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RemoteStatusResponse {
    pub state: RemoteState,
    // The provider's current player domain, as a bare hostname. Absent for
    // providers whose embed origin is still a constant on both sides.
    #[serde(default)]
    pub embed_domain: Option<String>,
    #[serde(default)]
    pub progress: Option<f64>,
    #[serde(default)]
    pub bytes_done: Option<u64>,
    #[serde(default)]
    pub bytes_total: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    // The provider's own status fields, echoed and sanitised by the Worker so an
    // unmapped state is visible in the log instead of a silent "uploading".
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProviderStatus {
    #[serde(default)]
    pub global_drive: bool,
    #[serde(default)]
    pub requested_drive: bool,
    #[serde(default)]
    pub lulustream: bool,
    #[serde(default)]
    pub voe: bool,
    #[serde(default)]
    pub byse: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StatusResponse {
    pub providers: ProviderStatus,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ErrorEnvelope {
    pub error: BrokerErrorBody,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BrokerErrorBody {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reported_domain_replaces_the_fallback_host() {
        assert_eq!(
            RemoteProvider::Byse.final_url_on(Some("moved.example"), "abc123"),
            "https://moved.example/e/abc123"
        );
        assert_eq!(
            RemoteProvider::Byse.final_url_on(None, "abc123"),
            "https://byse.sx/e/abc123"
        );
    }

    // The provider supplies this string, so anything that could point a published
    // link somewhere other than the host it names has to fall back instead.
    #[test]
    fn only_a_bare_hostname_is_accepted() {
        for rejected in [
            "https://evil.example",
            "evil.example/path",
            "evil.example:8443",
            "user@evil.example",
            "evil",
            "",
            "-evil.example",
            "evil-.example",
            "EVIL.example",
        ] {
            assert_eq!(
                RemoteProvider::Byse.final_url_on(Some(rejected), "abc"),
                "https://byse.sx/e/abc",
                "{rejected} must not reach a published link"
            );
        }
        assert_eq!(valid_embed_domain("a-b.c-d.example"), Some("a-b.c-d.example"));
    }
}
