use std::path::PathBuf;

use tokio::fs::{create_dir_all, remove_dir_all, rename};

use crate::pnworker::core::Job;
use crate::pnworker::frontend::Frontend;
use crate::pnworker::messages::MessagePayload;

pub(crate) async fn render(job: &mut Job, payload: MessagePayload) {
    let mut fe = std::mem::replace(&mut job.frontend, Frontend::None);
    fe.update(job, &payload).await;
    job.frontend = fe;
}

pub(crate) async fn cleanup_job(source: &PathBuf, dest: &PathBuf) {
    create_dir_all(dest).await.ok();
    let _ = rename(
        source.join("contents").join("subtitle.ass"),
        dest.join("subtitle.ass"),
    )
    .await;
    let _ = rename(
        source.join("contents").join("fetch.torrent"),
        dest.join("fetch.torrent"),
    )
    .await;
    let _ = rename(source.join("log"), dest.join("log")).await;
    remove_dir_all(source).await.ok();
}
