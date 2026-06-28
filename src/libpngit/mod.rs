pub mod core;

pub use core::{
    attach_repo, destruct_repo, detach_channel, init_repo, list_attachments, set_source,
    smartcode_merge, Attachment, Credits, DestructOutcome, DetachOutcome, RepoOutcome,
    SmartMergeResult, SourceOutcome,
};
