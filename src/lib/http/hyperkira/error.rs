use reqwest::StatusCode;
use std::fmt;

pub type AkiraResult<T> = Result<T, AkiraError>;

#[derive(Debug)]
pub enum AkiraError {
    Http(reqwest::Error),
    Api {
        status: StatusCode,
        body: String,
    },
    Decode {
        source: serde_json::Error,
        body: String,
    },
}

impl fmt::Display for AkiraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AkiraError::Http(err) => write!(f, "akira request failed: {}", err),
            AkiraError::Api { status, body } => write!(f, "akira returned {}: {}", status, body),
            AkiraError::Decode { source, body } => {
                write!(
                    f,
                    "akira response decode failed: {} (body: {})",
                    source, body
                )
            }
        }
    }
}

impl std::error::Error for AkiraError {}

impl From<reqwest::Error> for AkiraError {
    fn from(value: reqwest::Error) -> Self {
        AkiraError::Http(value)
    }
}
