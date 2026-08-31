pub mod core;

pub const README_BASE_GUIDE: &str = include_str!("readme_guide.md");

pub use core::{
    attach_repo, attachment_health, destruct_repo, detach_channel, init_repo, list_attachments,
    record_attachment_sync, set_source,
    smartcode_merge, Attachment, Credits, DestructOutcome, DetachOutcome, RepoOutcome,
    SmartMergeResult, SourceOutcome,
};
