use std::path::{Path, PathBuf};

// Where a job's log directory was found. A job keeps its logs under DB/work
// while it runs and lifecycle.rs moves them to DB/saved_data when it archives,
// so both places have to be searched before deciding a job has no logs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobLogLocation {
    Active,
    Archived,
}

impl JobLogLocation {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobLogLocation::Active => "active",
            JobLogLocation::Archived => "archived",
        }
    }
}

#[derive(Clone, Debug)]
pub struct JobLogFile {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    // Unix seconds; None when the filesystem cannot report a modification time.
    pub modified: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct JobLogs {
    pub location: JobLogLocation,
    pub directory: PathBuf,
    pub files: Vec<JobLogFile>,
}

impl JobLogs {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes).sum()
    }

    pub fn file(&self, name: &str) -> Option<&JobLogFile> {
        self.files.iter().find(|file| file.name == name)
    }
}

pub fn active_log_dir(job_id: u64) -> PathBuf {
    PathBuf::from("DB").join("work").join(job_id.to_string()).join("log")
}

pub fn archived_log_dir(job_id: u64) -> PathBuf {
    PathBuf::from("DB")
        .join("saved_data")
        .join(job_id.to_string())
        .join("log")
}

// Returns the first non-empty log directory for the job, preferring the live
// one. An unreadable directory is remembered but not fatal: the archived copy
// may still answer, and only when nothing readable turns up does the error win.
pub async fn find_job_logs(job_id: u64) -> Result<Option<JobLogs>, String> {
    let mut last_error = None;
    for (directory, location) in [
        (active_log_dir(job_id), JobLogLocation::Active),
        (archived_log_dir(job_id), JobLogLocation::Archived),
    ] {
        let files = match log_files(&directory).await {
            Ok(files) => files,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        if files.is_empty() {
            continue;
        }
        return Ok(Some(JobLogs {
            location,
            directory,
            files,
        }));
    }
    match last_error {
        Some(error) => Err(error),
        None => Ok(None),
    }
}

// Plain files only, sorted by name; subdirectories (tool scratch space) are
// skipped so the archive stays flat.
pub async fn log_files(directory: &Path) -> Result<Vec<JobLogFile>, String> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut files = Vec::new();
    // One unreadable entry used to fail the whole listing, which is the opposite of what this is
    // for: a job whose directory holds a name its file no longer answers to would report *no* logs
    // at all — a `500` in place of every other transcript sitting right beside it. Remember the
    // problem, name it, and let it decide the outcome only when nothing readable turned up.
    let mut problem: Option<String> = None;
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(error) => {
                problem.get_or_insert(format!("{}: {}", directory.display(), error));
                break;
            }
        };
        let metadata = match entry.metadata().await {
            Ok(metadata) => metadata,
            Err(error) => {
                problem.get_or_insert(format!("{}: {}", entry.path().display(), error));
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }
        let path = entry.path();
        let name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_secs());
        files.push(JobLogFile {
            name,
            path,
            bytes: metadata.len(),
            modified,
        });
    }
    if let Some(problem) = problem {
        if files.is_empty() {
            return Err(problem);
        }
        eprintln!("[Pandora] job log listing skipped an unreadable entry: {}", problem);
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}

pub async fn zip_log_files(files: &[JobLogFile]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut writer = async_zip::base::write::ZipFileWriter::new(&mut out);
        for file in files {
            let bytes = tokio::fs::read(&file.path)
                .await
                .map_err(|error| format!("{}: {}", file.path.display(), error))?;
            let entry = async_zip::ZipEntryBuilder::new(
                file.name.clone().into(),
                async_zip::Compression::Deflate,
            );
            writer
                .write_entry_whole(entry, &bytes)
                .await
                .map_err(|error| error.to_string())?;
        }
        writer.close().await.map_err(|error| error.to_string())?;
    }
    Ok(out)
}

pub struct JobLogText {
    pub text: String,
    // Size of the file on disk, before any tail/byte trimming.
    pub bytes: u64,
    pub truncated: bool,
}

// Reads one log file for display. Encoder logs grow without bound, so the read
// starts `max_bytes` from the end (dropping a partial first line) and `tail`
// further narrows it to the last N lines — the interesting part of a stuck job
// is always at the end.
pub async fn read_job_log(
    file: &JobLogFile,
    max_bytes: u64,
    tail: Option<usize>,
) -> Result<JobLogText, String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut handle = tokio::fs::File::open(&file.path)
        .await
        .map_err(|error| format!("{}: {}", file.path.display(), error))?;
    let size = handle
        .metadata()
        .await
        .map_err(|error| error.to_string())?
        .len();
    let truncated = size > max_bytes;
    if truncated {
        handle
            .seek(std::io::SeekFrom::Start(size - max_bytes))
            .await
            .map_err(|error| error.to_string())?;
    }
    let mut raw = Vec::with_capacity(max_bytes.min(size) as usize);
    handle
        .read_to_end(&mut raw)
        .await
        .map_err(|error| error.to_string())?;

    let mut text = String::from_utf8_lossy(&raw).into_owned();
    if truncated {
        // The seek landed mid-line; drop the fragment so the output starts clean.
        if let Some(newline) = text.find('\n') {
            text = text[newline + 1..].to_string();
        }
    }
    if let Some(tail) = tail {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() > tail {
            text = lines[lines.len() - tail..].join("\n");
        }
    }
    Ok(JobLogText {
        text,
        bytes: size,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "pandora-joblog-{}-{}",
            std::process::id(),
            name
        ));
        tokio::fs::remove_dir_all(&root).await.ok();
        tokio::fs::create_dir_all(&root).await.unwrap();
        root
    }

    #[tokio::test]
    async fn log_files_lists_only_files_and_zips_them() {
        let root = scratch("zip").await;
        tokio::fs::create_dir_all(root.join("nested")).await.unwrap();
        tokio::fs::write(root.join("encode.log"), b"encode log").await.unwrap();
        tokio::fs::write(root.join("upload.log"), b"upload log").await.unwrap();

        let files = log_files(&root).await.unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "encode.log");
        assert_eq!(files[0].bytes, 10);
        let archive = zip_log_files(&files).await.unwrap();
        assert!(archive.starts_with(b"PK"));

        tokio::fs::remove_dir_all(root).await.ok();
    }

    #[tokio::test]
    async fn missing_directory_is_empty_not_an_error() {
        let missing = std::env::temp_dir().join("pandora-joblog-does-not-exist");
        assert!(log_files(&missing).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn read_job_log_tails_lines_and_bytes() {
        let root = scratch("read").await;
        let path = root.join("PNmpeg_Encode1.log");
        tokio::fs::write(&path, b"one\ntwo\nthree\nfour\n").await.unwrap();
        let files = log_files(&root).await.unwrap();
        let file = &files[0];

        let whole = read_job_log(file, 1024, None).await.unwrap();
        assert_eq!(whole.text, "one\ntwo\nthree\nfour\n");
        assert_eq!(whole.bytes, 19);
        assert!(!whole.truncated);

        let tailed = read_job_log(file, 1024, Some(2)).await.unwrap();
        assert_eq!(tailed.text, "three\nfour");

        // A byte cap starts mid-line, so the partial line is dropped.
        let capped = read_job_log(file, 10, None).await.unwrap();
        assert!(capped.truncated);
        assert_eq!(capped.text, "four\n");
        assert_eq!(capped.bytes, 19);

        tokio::fs::remove_dir_all(root).await.ok();
    }

    #[tokio::test]
    async fn an_entry_that_does_not_resolve_never_hides_the_logs_beside_it() {
        let root = scratch("dangling").await;
        tokio::fs::write(root.join("PNmpeg_Encode1.log"), b"why it failed").await.unwrap();
        // A name the directory lists that resolves to nothing. `DirEntry::metadata` is an lstat, so
        // this one is merely "not a file"; production found a bind mount that fails the stat itself.
        // Either way the transcript sitting beside it has to survive the encounter.
        std::os::unix::fs::symlink(root.join("gone.log"), root.join("dangling.log")).unwrap();

        let files = log_files(&root).await.unwrap();
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(files[0].name, "PNmpeg_Encode1.log");

        // And on its own it reads as a job with no logs, not as a failed request.
        tokio::fs::remove_file(root.join("PNmpeg_Encode1.log")).await.unwrap();
        assert!(log_files(&root).await.unwrap().is_empty());
        tokio::fs::remove_dir_all(&root).await.ok();
    }
}
