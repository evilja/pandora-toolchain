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
    Doodstream,
    Lulustream,
    Voe,
}

impl RemoteProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Doodstream => "Doodstream",
            Self::Lulustream => "Lulustream",
            Self::Voe => "Voe",
        }
    }

    pub fn final_url(self, file_code: &str) -> String {
        match self {
            Self::Doodstream => format!("https://doodstream.com/e/{file_code}"),
            Self::Lulustream => format!("https://luluvdo.com/e/{file_code}"),
            Self::Voe => format!("https://voe.sx/e/{file_code}"),
        }
    }
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
    #[serde(default)]
    pub progress: Option<f64>,
    #[serde(default)]
    pub bytes_done: Option<u64>,
    #[serde(default)]
    pub bytes_total: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProviderStatus {
    #[serde(default)]
    pub global_drive: bool,
    #[serde(default)]
    pub requested_drive: bool,
    #[serde(default)]
    pub doodstream: bool,
    #[serde(default)]
    pub lulustream: bool,
    #[serde(default)]
    pub voe: bool,
    #[serde(default)]
    pub abyss: bool,
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
