pub mod core;

pub const README_BASE_GUIDE: &str = include_str!("readme_guide.md");

pub use core::{
    attach_repo, destruct_repo, detach_channel, init_repo, list_attachments, set_source,
    smartcode_merge, Attachment, Credits, DestructOutcome, DetachOutcome, RepoOutcome,
    SmartMergeResult, SourceOutcome,
};
