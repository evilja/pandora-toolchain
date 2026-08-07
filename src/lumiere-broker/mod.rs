mod client;
mod protocol;
mod transfer;
mod upload;

pub use client::{Config, Error, ErrorKind, LumiereClient};
pub use protocol::{DriveCandidate, ProviderStatus, RemoteProvider};
pub use transfer::serve_transfer;
pub use upload::{
    DriveUploadResult, DriveUploadSpec, RemoteUploadResult, RemoteUploadSpec, UploadError,
    UploadProgress, content_type_for_path,
};

pub const GLOBAL_DRIVE_PROFILE: &str = "global";

pub fn guild_drive_profile(server_id: u64) -> String {
    format!("guild:{server_id}")
}
