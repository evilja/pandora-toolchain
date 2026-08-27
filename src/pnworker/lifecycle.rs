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
    preserve_log_dir(&source.join("log"), &dest.join("log")).await;
    remove_dir_all(source).await.ok();
}

// The log directory is the only durable account of why a job ended, and this is the last moment it
// exists: the wipe below is unconditional. A single `rename` is not enough to carry it across —
// it refuses when something is already at the destination, and cannot cross a mount point at all,
// and either way the logs went into `remove_dir_all` unreported. Fall back to moving the files one
// at a time, and say so when even that fails.
async fn preserve_log_dir(source: &PathBuf, dest: &PathBuf) {
    if !tokio::fs::metadata(source).await.is_ok_and(|meta| meta.is_dir()) {
        return;
    }
    let refused = match rename(source, dest).await {
        Ok(()) => return,
        Err(e) => e,
    };
    // Which of the two reasons it was decides whether the per-file fallback is a rare event or the
    // normal path, and only production can say. Name it rather than falling back in silence.
    eprintln!(
        "[Pandora] job log directory {} could not be moved to {} ({}); moving its files one at a time",
        source.display(),
        dest.display(),
        refused
    );
    if let Err(e) = create_dir_all(dest).await {
        eprintln!("[Pandora] job logs at {} could not be preserved: {}", source.display(), e);
        return;
    }
    let Ok(mut entries) = tokio::fs::read_dir(source).await else {
        eprintln!("[Pandora] job logs at {} could not be listed", source.display());
        return;
    };
    // Read the whole listing before moving anything. Taking entries out of a directory while still
    // iterating it is undefined enough on a local filesystem, and `DB` is a bind mount in
    // production, where the same walk can start reporting names it can no longer stat.
    let mut names = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        names.push(entry.file_name());
    }
    drop(entries);
    for name in names {
        let from = source.join(&name);
        let target = dest.join(&name);
        if rename(&from, &target).await.is_ok() {
            continue;
        }
        if let Err(e) = tokio::fs::copy(&from, &target).await {
            eprintln!("[Pandora] job log {} could not be preserved: {}", from.display(), e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archiving_keeps_logs_even_when_the_destination_already_holds_some() {
        let root = std::env::temp_dir().join(format!("pandora-cleanup-logs-{}", std::process::id()));
        tokio::fs::remove_dir_all(&root).await.ok();
        let source = root.join("work").join("777");
        let dest = root.join("saved_data").join("777");

        tokio::fs::create_dir_all(source.join("log")).await.unwrap();
        tokio::fs::write(source.join("log").join("PNmpeg_Encode777.log"), b"why it failed")
            .await
            .unwrap();
        // A publish, or a gitsync that ran while this job was still going, gets there first: the
        // directory rename then fails and used to take the transcript down with it.
        tokio::fs::create_dir_all(dest.join("log")).await.unwrap();
        tokio::fs::write(dest.join("log").join("publish.log"), b"earlier")
            .await
            .unwrap();

        cleanup_job(&source, &dest).await;

        assert_eq!(
            tokio::fs::read_to_string(dest.join("log").join("PNmpeg_Encode777.log")).await.unwrap(),
            "why it failed"
        );
        assert_eq!(
            tokio::fs::read_to_string(dest.join("log").join("publish.log")).await.unwrap(),
            "earlier"
        );
        assert!(!source.exists());
        tokio::fs::remove_dir_all(&root).await.ok();
    }
}
