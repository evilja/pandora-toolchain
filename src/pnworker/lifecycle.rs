use std::path::PathBuf;

use tokio::fs::{create_dir_all, remove_dir_all, rename};

use crate::pnworker::core::{Job, Stage};
use crate::pnworker::frontend::Frontend;
use crate::pnworker::messages::{format_message, MessagePayload, FAILURE_TAIL};

// Every user-visible effect in the worker goes through here, which is exactly why the link's node
// side taps it: declines and cancellations never reach a `CommData` stream, so hooking the message
// bus would have reported a node's encode progress faithfully and lost the reason a job was
// refused. On a coordinator the report call returns immediately.
pub(crate) async fn render(job: &mut Job, payload: MessagePayload) {
    // A failed job's message says *what* failed; the reason is in the log the tool wrote, which
    // `cleanup_job` moves into `saved_data` moments later and which otherwise only `/catlogs` can
    // reach. Attaching the tail here puts the reason where the failure is reported — and this is
    // the one place every user-visible update passes through, so no failure path can miss it.
    // The tail is appended once-per-job rather than per render: a retried probe or a stage that
    // reports twice must not stack copies of the same lines.
    if job.ready == Stage::Failed {
        if let Some(tail) = failure_tail(job).await {
            if !job.encode_warnings.iter().any(|warning| warning.contains(&tail)) {
                job.encode_warnings.push(tail);
            }
        }
    }
    crate::pnworker::metrics::observe(job, &payload).await;
    let mut fe = std::mem::replace(&mut job.frontend, Frontend::None);
    fe.update(job, &payload).await;
    job.frontend = fe;
    crate::pnworker::link::client::report(job.job_id, &payload, job.ready, &job.worker);
}

// How much of the newest log to read and how many of its lines to keep. Tools write progress
// continuously, so the answer is always at the end, and a finished encode log can be megabytes.
const FAILURE_TAIL_BYTES: u64 = 4096;
const FAILURE_TAIL_LINES: usize = 5;

// The last few lines the failing tool wrote, labelled in the job's language. `None` covers every
// case where there is nothing honest to show: no log directory, an empty log, or a job that failed
// before a tool ever ran.
async fn failure_tail(job: &Job) -> Option<String> {
    let tail = read_log_tail(&job.directory.join("log")).await?;
    Some(format_message(FAILURE_TAIL, &job.lang, &[tail]))
}

async fn read_log_tail(directory: &std::path::Path) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut files = crate::lib::joblog::log_files(directory).await.ok()?;
    // Newest last: the tool that failed is almost always the last one to have written anything — a
    // job that died in the encoder already has a download log beside it.
    // `JobLogFile::modified` is whole seconds, which ties for a download that was replaced by an
    // encode in the same second and leaves the name ordering to decide — that is how the wrong
    // tool's log gets reported. Stat each file again for a sub-second timestamp to break ties; the
    // table view keeps the coarse value it wants, and a directory holds a handful of files.
    let mut stamped = Vec::with_capacity(files.len());
    for file in files.drain(..) {
        let stamp = tokio::fs::metadata(&file.path)
            .await
            .and_then(|meta| meta.modified())
            .ok();
        stamped.push((stamp, file));
    }
    stamped.sort_by_key(|(stamp, _)| *stamp);
    let (_, newest) = stamped.pop()?;
    let mut file = tokio::fs::File::open(&newest.path).await.ok()?;
    let len = file.metadata().await.ok()?.len();
    if len == 0 {
        return None;
    }
    file.seek(std::io::SeekFrom::Start(len.saturating_sub(FAILURE_TAIL_BYTES)))
        .await
        .ok()?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await.ok()?;
    // A byte offset can land in the middle of a UTF-8 sequence; lossy decoding drops that one
    // character rather than refusing to show the lines after it.
    let text = String::from_utf8_lossy(&buffer);
    let tail = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(FAILURE_TAIL_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    if tail.trim().is_empty() {
        return None;
    }
    Some(tail)
}

#[cfg(test)]
mod failure_tail_tests {
    use super::read_log_tail;

    fn scratch(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "pandora-failure-tail-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    // The reason a job died is the last thing its tool wrote, so the newest log's final lines are
    // the ones worth showing — not the first few of whichever file sorted first.
    #[tokio::test]
    async fn the_newest_log_wins_and_only_its_last_lines_are_kept() {
        let root = scratch("newest");
        std::fs::write(root.join("PNp2p1.log"), "old tool\nfailed early\n").unwrap();
        let mut lines = String::new();
        for i in 0..40 {
            lines.push_str(&format!("encode line {i}\n"));
        }
        std::fs::write(root.join("PNmpeg2.log"), lines).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        tokio::fs::write(root.join("PNmpeg3.log"), "ffmpeg: unknown encoder 'libx264'\n").await.unwrap();

        let tail = read_log_tail(&root).await.unwrap();
        assert!(tail.contains("unknown encoder"), "{tail}");
        assert!(!tail.contains("old tool"), "{tail}");

        std::fs::remove_dir_all(root).ok();
    }

    // Nothing to show is a real answer: a job that failed before any tool ran, or one whose log
    // directory was already moved, must not produce a heading with an empty body under it.
    #[tokio::test]
    async fn an_empty_or_missing_log_directory_yields_nothing() {
        let root = scratch("empty");
        tokio::fs::write(root.join("PNmpeg9.log"), "").await.unwrap();
        assert!(read_log_tail(&root).await.is_none());
        assert!(read_log_tail(&root.join("absent")).await.is_none());
        std::fs::remove_dir_all(root).ok();
    }
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
    if let Some(id) = source.file_name().and_then(|v| v.to_str()).and_then(|v| v.parse().ok()) {
        crate::pnworker::metrics::forget(id).await;
    }
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
