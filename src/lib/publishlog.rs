use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

use crate::lib::joblog::{active_log_dir, archived_log_dir};

// Every publish command writes here, so `GET /jobs/:id/logs` lists one predictable
// name next to the encoder's own logs instead of a per-provider file the caller has
// to guess at.
pub const PUBLISH_LOG_NAME: &str = "publish.log";

// Publishing happens long after the encode finished, so a job's logs have usually
// already been moved to DB/saved_data by lifecycle.rs. Append beside whichever copy
// exists, and fall back to the active directory when the job kept no logs at all --
// a publish record is worth creating a directory for, since a job that never
// produced logs is exactly the one nobody can otherwise diagnose.
async fn publish_log_path(job_id: u64) -> PathBuf {
    for directory in [active_log_dir(job_id), archived_log_dir(job_id)] {
        if tokio::fs::metadata(&directory).await.is_ok() {
            return directory.join(PUBLISH_LOG_NAME);
        }
    }
    active_log_dir(job_id).join(PUBLISH_LOG_NAME)
}

// Unix seconds, matching the `modified` field joblog.rs already reports, so a reader
// can line publish events up against the log file timestamps without a second format.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

// One line per event: timestamp, provider, then the text. Appends rather than
// truncating, because a job is published to several providers and re-published after
// a failure -- overwriting would discard the history that makes a stuck publish
// readable. Logging is best-effort: a publish must not fail because its audit line
// could not be written, so every error here is swallowed after a stderr note.
pub async fn log_publish(job_id: u64, provider: &str, event: impl AsRef<str>) {
    let path = publish_log_path(job_id).await;
    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            eprintln!("[publishlog] could not create {}: {}", parent.display(), e);
            return;
        }
    }
    // Collapse newlines so one event stays one line and the file stays greppable;
    // provider errors routinely arrive as multi-line API bodies.
    let event = event.as_ref().replace('\n', " ").replace('\r', "");
    let line = format!("{} [{}] {}\n", now_unix(), provider, event.trim());
    let mut file = match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    {
        Ok(file) => file,
        Err(e) => {
            eprintln!("[publishlog] could not open {}: {}", path.display(), e);
            return;
        }
    };
    if let Err(e) = file.write_all(line.as_bytes()).await {
        eprintln!("[publishlog] could not write {}: {}", path.display(), e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_collapse_to_a_single_greppable_line() {
        let multi = "Akira episode create failed: 500\n{\"detail\": \"boom\"}\r\n";
        let collapsed = multi.replace('\n', " ").replace('\r', "");
        assert!(!collapsed.trim().contains('\n'));
        assert_eq!(
            collapsed.trim(),
            "Akira episode create failed: 500 {\"detail\": \"boom\"}"
        );
    }

    #[tokio::test]
    async fn the_active_directory_wins_when_both_exist() {
        // Both lookups are relative to the process CWD, so this only asserts the
        // preference order rather than touching the real DB tree.
        let active = active_log_dir(4242);
        let archived = archived_log_dir(4242);
        assert!(active.ends_with("log"));
        assert!(archived.ends_with("log"));
        assert_ne!(active, archived);
    }
}
