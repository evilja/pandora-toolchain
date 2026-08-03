use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;

use crate::lib::protocol::core::{Protocol, Schema};
use crate::lib::torrent::{
    DownloadEvent, DownloadOptions, FileSelection, TorrentClient, TorrentError, TorrentSource,
};
use crate::{lib_pn_data, lib_pn_emit, lib_pn_schema};

pub use crate::lib::torrent::{magnet_info_hash, torrent_info_hash};

const MAX_TORRENT_FILE_SIZE: u64 = 64 * 1024 * 1024;

pub struct P2p {
    client: TorrentClient,
    cfile: Option<PathBuf>,
}

struct DownloadLock {
    path: PathBuf,
    token: String,
}

fn is_video_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mkv" | "mp4" | "m4v" | "mov" | "avi" | "webm" | "ts" | "m2ts"
    )
}

fn is_video_name(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(is_video_ext)
        .unwrap_or(false)
}

pub async fn cleanup_torrent_runtime() {
    let root = lock_root();
    let mut entries = match fs::read_dir(&root).await {
        Ok(entries) => entries,
        Err(_) => return,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.path().extension().and_then(|value| value.to_str()) == Some("lock") {
            fs::remove_file(entry.path()).await.ok();
        }
    }
    fs::remove_dir(&root).await.ok();
}

impl P2p {
    pub async fn new(cfile: Option<String>) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            client: TorrentClient::from_env()?,
            cfile: cfile.map(PathBuf::from),
        })
    }

    pub async fn probe_torrent(
        &self,
        torrent_path: &str,
        srcmgn: bool,
        _tag: Option<String>,
    ) -> Result<Vec<(u64, String, u64)>, Box<dyn std::error::Error>> {
        let source = torrent_source(torrent_path, srcmgn);
        let files = self.client.probe(&source).await?;
        Ok(files
            .into_iter()
            .filter(|file| is_video_name(&file.path))
            .map(|file| (file.index, portable_path(&file.path), file.length))
            .collect())
    }

    pub async fn download_selected(
        &self,
        torrent_path: &str,
        save_path: &str,
        file_indices: Vec<u64>,
        proto: &Protocol,
        neg: String,
        srcmgn: bool,
        _tag: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source = torrent_source(torrent_path, srcmgn);
        let _lock = DownloadLock::acquire(&source, save_path).await?;
        let options = DownloadOptions {
            selection: FileSelection::Only(file_indices),
            cancel_file: self.cfile.clone(),
        };
        let mut last_progress = None;
        let result = self
            .client
            .download(&source, save_path, options, |event| match event {
                DownloadEvent::FileSelected { path, .. } => {
                    let name = portable_path(&path);
                    println!(
                        "{}",
                        lib_pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, leaf],
                            data = ["4", name]
                        )
                        .unwrap()
                    );
                }
                DownloadEvent::Progress {
                    downloaded_bytes,
                    total_bytes,
                    percent,
                } => emit_progress_throttled(
                    proto,
                    &neg,
                    percent,
                    downloaded_bytes,
                    total_bytes,
                    &mut last_progress,
                ),
                DownloadEvent::Complete => emit_done(proto, &neg),
                DownloadEvent::Metadata { .. } => {}
            })
            .await;
        finish_download(result, proto, &neg)
    }

    pub async fn download_and_remove(
        &self,
        torrent_path: &str,
        save_path: &str,
        proto: &Protocol,
        neg: String,
        srcmgn: bool,
        _tag: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source = torrent_source(torrent_path, srcmgn);
        let _lock = DownloadLock::acquire(&source, save_path).await?;
        let options = DownloadOptions {
            selection: FileSelection::All,
            cancel_file: self.cfile.clone(),
        };
        let mut last_progress = None;
        let result = self
            .client
            .download(&source, save_path, options, |event| match event {
                DownloadEvent::Progress {
                    downloaded_bytes,
                    total_bytes,
                    percent,
                } => emit_progress_throttled(
                    proto,
                    &neg,
                    percent,
                    downloaded_bytes,
                    total_bytes,
                    &mut last_progress,
                ),
                DownloadEvent::Complete => emit_done(proto, &neg),
                DownloadEvent::Metadata { .. } | DownloadEvent::FileSelected { .. } => {}
            })
            .await;
        finish_download(result, proto, &neg)
    }
}

impl DownloadLock {
    async fn acquire(
        source: &TorrentSource,
        save_path: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let hash = source_hash(source).await?;
        let root = lock_root();
        fs::create_dir_all(&root).await?;
        let path = root.join(format!("{hash}.lock"));
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let token = format!("{}-{nonce}", std::process::id());
        for _ in 0..2 {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
            {
                Ok(mut file) => {
                    file.write_all(format!("{token}\n{save_path}").as_bytes())
                        .await?;
                    file.flush().await?;
                    return Ok(Self { path, token });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path).await {
                        fs::remove_file(&path).await.ok();
                        continue;
                    }
                    let owner = fs::read_to_string(&path)
                        .await
                        .ok()
                        .and_then(|value| value.split_once('\n').map(|(_, path)| path.to_string()))
                        .unwrap_or_default();
                    return Err(format!("DUPLICATE_TORRENT|{owner}").into());
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(format!("DUPLICATE_TORRENT|{save_path}").into())
    }
}

impl Drop for DownloadLock {
    fn drop(&mut self) {
        let owned = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|value| value.lines().next().map(str::to_string))
            .is_some_and(|token| token == self.token);
        if owned {
            std::fs::remove_file(&self.path).ok();
        }
    }
}

fn torrent_source(path: &str, magnet: bool) -> TorrentSource {
    if magnet {
        TorrentSource::Magnet(path.to_string())
    } else {
        TorrentSource::File(PathBuf::from(path))
    }
}

async fn source_hash(source: &TorrentSource) -> Result<String, Box<dyn std::error::Error>> {
    match source {
        TorrentSource::Magnet(value) => {
            magnet_info_hash(value).ok_or_else(|| "magnet link has no valid v1 info hash".into())
        }
        TorrentSource::File(path) => {
            if fs::metadata(path).await?.len() > MAX_TORRENT_FILE_SIZE {
                return Err("torrent file exceeds the 64 MiB limit".into());
            }
            let bytes = fs::read(path).await?;
            if bytes.len() as u64 > MAX_TORRENT_FILE_SIZE {
                return Err("torrent file exceeds the 64 MiB limit".into());
            }
            torrent_info_hash(&bytes)
                .ok_or_else(|| "torrent file has no valid info dictionary".into())
        }
        TorrentSource::Bytes(bytes) => torrent_info_hash(bytes)
            .ok_or_else(|| "torrent bytes have no valid info dictionary".into()),
    }
}

async fn lock_is_stale(path: &Path) -> bool {
    let old = fs::metadata(path)
        .await
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > Duration::from_secs(24 * 60 * 60));
    if old {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        let pid = fs::read_to_string(path)
            .await
            .ok()
            .and_then(|value| value.lines().next()?.split_once('-')?.0.parse::<u32>().ok());
        return pid.is_some_and(|pid| !PathBuf::from(format!("/proc/{pid}")).exists());
    }
    #[cfg(not(target_os = "linux"))]
    false
}

fn lock_root() -> PathBuf {
    std::env::temp_dir().join("pandora-torrent-locks")
}

fn portable_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn emit_progress_throttled(
    proto: &Protocol,
    neg: &str,
    percent: f64,
    downloaded_bytes: u64,
    total_bytes: u64,
    last_progress: &mut Option<Instant>,
) {
    let now = Instant::now();
    if percent < 100.0
        && last_progress.is_some_and(|last| now.duration_since(last) < Duration::from_secs(5))
    {
        return;
    }
    *last_progress = Some(now);
    let percent = display_percent(percent, downloaded_bytes, total_bytes);
    println!(
        "{}",
        lib_pn_emit!(
            protocol = proto,
            negkey = neg,
            schema = [leaf, [leaf, leaf, leaf]],
            data = ["0", [percent, downloaded_bytes, total_bytes]]
        )
        .unwrap()
    );
}

// Progress remains below 100 until the torrent client reports every byte written.
fn display_percent(percent: f64, downloaded_bytes: u64, total_bytes: u64) -> f64 {
    if total_bytes > 0 && downloaded_bytes >= total_bytes {
        return 100.0;
    }

    percent.floor().clamp(0.0, 99.0)
}

fn emit_done(proto: &Protocol, neg: &str) {
    println!(
        "{}",
        lib_pn_emit!(
            protocol = proto,
            negkey = neg,
            schema = [leaf, leaf],
            data = ["1", "DONE"]
        )
        .unwrap()
    );
}

fn finish_download(
    result: crate::lib::torrent::Result<crate::lib::torrent::DownloadSummary>,
    proto: &Protocol,
    neg: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match result {
        Ok(_) => Ok(()),
        Err(TorrentError::Cancelled) => {
            println!(
                "{}",
                lib_pn_emit!(
                    protocol = proto,
                    negkey = neg,
                    schema = [leaf, leaf],
                    data = ["3", "CANCELFILE"]
                )
                .unwrap()
            );
            Ok(())
        }
        Err(error) => Err(Box::new(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_extensions_are_case_insensitive() {
        assert!(is_video_name(Path::new("episode.MKV")));
        assert!(is_video_name(Path::new("episode.m2ts")));
        assert!(!is_video_name(Path::new("episode.ass")));
    }

    #[test]
    fn portable_paths_use_forward_slashes() {
        assert_eq!(
            portable_path(Path::new("show/episode.mkv")),
            "show/episode.mkv"
        );
    }

    #[test]
    fn progress_does_not_round_incomplete_downloads_up_to_100() {
        assert_eq!(display_percent(99.72, 1_426, 1_430), 99.0);
        assert_eq!(display_percent(100.0, 1_429, 1_430), 99.0);
        assert_eq!(display_percent(100.0, 1_430, 1_430), 100.0);
    }

    #[tokio::test]
    async fn download_locks_report_the_owner_and_release_on_drop() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string();
        let bytes = format!("d4:infod4:name{}:{}ee", nonce.len(), nonce).into_bytes();
        let source = TorrentSource::Bytes(bytes);
        let first = DownloadLock::acquire(&source, "/first/save").await.unwrap();
        let duplicate = match DownloadLock::acquire(&source, "/second/save").await {
            Ok(_) => panic!("duplicate lock unexpectedly succeeded"),
            Err(error) => error.to_string(),
        };
        assert_eq!(duplicate, "DUPLICATE_TORRENT|/first/save");
        drop(first);
        let second = DownloadLock::acquire(&source, "/second/save")
            .await
            .unwrap();
        drop(second);
    }
}
