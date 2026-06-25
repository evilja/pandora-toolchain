use crate::libpnprotocol::core::{Protocol, Schema};
use crate::{lib_pn_data, lib_pn_emit, lib_pn_schema};
use qbit_rs::Qbit;
use qbit_rs::model::Priority;
use qbit_rs::model::{
    AddTorrentArg, Credential, GetTorrentListArg, Sep, State, Torrent, TorrentFile, TorrentSource,
};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tokio::fs::{self, try_exists};
use tokio::time::{Duration, sleep};

pub struct P2p {
    api: Qbit,
    cfile: Option<PathBuf>,
}

fn is_video_ext(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "mkv" | "mp4" | "m4v" | "mov" | "avi" | "webm" | "ts" | "m2ts")
}

fn is_video_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(is_video_ext)
        .unwrap_or(false)
}

impl P2p {
    pub async fn new(host: &str, uname: &str, pass: &str, cfile: Option<String>) -> Self {
        println!(
            "[pnp2p] new() called with host={}, uname={}, cfile={:?}",
            host, uname, cfile
        );
        let credential = Credential::new(uname, pass);
        Self {
            api: Qbit::new(host, credential),
            cfile: cfile.map(PathBuf::from),
        }
    }

    pub async fn probe_torrent(
        &self,
        torrent_path: &str,
        srcmgn: bool,
    ) -> Result<Vec<(u64, String, u64)>, Box<dyn std::error::Error>> {
        println!(
            "[pnp2p] probe_torrent entry: torrent_path={}, srcmgn={}",
            torrent_path, srcmgn
        );
        let source = if srcmgn {
            println!("[pnp2p] probe_torrent: creating source from URL(s)");
            TorrentSource::Urls {
                urls: Sep::<reqwest::Url, '\n'>::from_str(torrent_path)?,
            }
        } else {
            println!("[pnp2p] probe_torrent: reading torrent file from disk");
            let torrent_bytes = fs::read(torrent_path).await?;
            TorrentSource::TorrentFiles {
                torrents: vec![TorrentFile {
                    filename: torrent_path.to_string(),
                    data: torrent_bytes,
                }],
            }
        };

        let temp_dir = std::env::temp_dir().join(format!("qb_probe_{}", std::process::id()));
        println!("[pnp2p] probe_torrent: creating temp dir {:?}", temp_dir);
        tokio::fs::create_dir_all(&temp_dir).await?;

        println!(
            "[pnp2p] probe_torrent: calling api.add_torrent with savepath {:?}",
            temp_dir
        );
        let add_args = AddTorrentArg::builder()
            .source(source)
            .paused("true".to_string())
            .savepath(temp_dir.to_str().unwrap().to_string())
            .build();

        self.api.add_torrent(add_args).await?;
        println!("[pnp2p] probe_torrent: add_torrent succeeded");
        sleep(Duration::from_secs(1)).await;

        // Find the hash (most recently added)
        println!("[pnp2p] probe_torrent: fetching torrent list to get hash");
        let hash = self
            .api
            .get_torrent_list(GetTorrentListArg::builder().build())
            .await?
            .iter()
            .max_by_key(|t| t.added_on.unwrap_or(0))
            .ok_or("No torrent found after add")?
            .hash
            .clone()
            .unwrap();
        println!("[pnp2p] probe_torrent: obtained hash = {}", hash);

        // Wait for metadata (poll until contents are available)
        println!("[pnp2p] probe_torrent: polling for metadata (max 30s)");
        let mut files = Vec::new();
        for i in 0..30 {
            sleep(Duration::from_secs(1)).await;
            let contents = self.api.get_torrent_contents(&hash, None).await?;
            println!(
                "[pnp2p] probe_torrent: poll {} -> {} files",
                i,
                contents.len()
            );
            if !contents.is_empty() {
                files = contents;
                break;
            }
        }
        if files.is_empty() {
            println!("[pnp2p] probe_torrent: metadata fetch failed, deleting torrent");
            self.api.delete_torrents(vec![hash], true).await?;
            return Err("Torrent metadata could not be fetched".into());
        }
        println!(
            "[pnp2p] probe_torrent: metadata fetched, {} total files",
            files.len()
        );

        let video_files: Vec<(u64, String, u64)> = files
            .iter()
            .filter(|f| is_video_name(&f.name))
            .map(|f| (f.index, f.name.clone(), f.size))
            .collect();
        println!(
            "[pnp2p] probe_torrent: found {} video files",
            video_files.len()
        );

        println!("[pnp2p] probe_torrent: deleting torrent and cleaning up temp dir");
        self.api.delete_torrents(vec![hash], true).await?;
        tokio::fs::remove_dir_all(&temp_dir).await.ok();
        println!("[pnp2p] probe_torrent: success, returning video_files");
        Ok(video_files)
    }

    pub async fn download_selected(
        &self,
        torrent_path: &str,
        save_path: &str,
        file_indices: Vec<u64>,
        proto: Protocol,
        neg: String,
        srcmgn: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        println!(
            "[pnp2p] download_selected entry: torrent_path={}, save_path={}, file_indices={:?}, srcmgn={}",
            torrent_path, save_path, file_indices, srcmgn
        );
        let source = if srcmgn {
            println!("[pnp2p] download_selected: creating source from URL(s)");
            TorrentSource::Urls {
                urls: Sep::<reqwest::Url, '\n'>::from_str(torrent_path)?,
            }
        } else {
            println!("[pnp2p] download_selected: reading torrent file from disk");
            let torrent_bytes = fs::read(torrent_path).await?;
            TorrentSource::TorrentFiles {
                torrents: vec![TorrentFile {
                    filename: torrent_path.to_string(),
                    data: torrent_bytes,
                }],
            }
        };

        println!("[pnp2p] download_selected: adding torrent paused");
        let add_args = AddTorrentArg::builder()
            .source(source)
            .paused("true".to_string())
            .savepath(host_save_path(save_path))
            .build();

        self.api.add_torrent(add_args).await?;
        println!("[pnp2p] download_selected: add_torrent succeeded, waiting 1s");
        sleep(Duration::from_secs(1)).await;

        println!("[pnp2p] download_selected: fetching torrent list to get hash");
        let torrents = self
            .api
            .get_torrent_list(GetTorrentListArg::builder().build())
            .await?;
        let hash = torrents
            .iter()
            .max_by_key(|t| t.added_on.unwrap_or(0))
            .ok_or("No torrents found")?
            .hash
            .clone()
            .unwrap();
        println!("[pnp2p] download_selected: obtained hash = {}", hash);

        // Wait for metadata and get file list
        println!("[pnp2p] download_selected: polling for metadata (max 30s)");
        let mut files = Vec::new();
        for i in 0..30 {
            sleep(Duration::from_secs(1)).await;
            let contents = self.api.get_torrent_contents(&hash, None).await?;
            println!(
                "[pnp2p] download_selected: poll {} -> {} files",
                i,
                contents.len()
            );
            if !contents.is_empty() {
                files = contents;
                break;
            }
        }

        if files.is_empty() {
            println!("[pnp2p] download_selected: metadata fetch failed, deleting torrent");
            self.api.delete_torrents(vec![hash.clone()], true).await?;
            return Err("Could not retrieve file list for priority setting".into());
        }
        println!(
            "[pnp2p] download_selected: metadata fetched, {} total files",
            files.len()
        );

        // Set priorities for selected files to NORMAL, others to DO_NOT_DOWNLOAD
        println!("[pnp2p] download_selected: setting file priorities");
        for file in &files {
            let priority = if file_indices.contains(&file.index) {
                let name = file.name.clone();
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
                Priority::Normal
            } else {
                Priority::DoNotDownload
            };
            println!(
                "[pnp2p] download_selected: file index={} name={} priority={:?}",
                file.index, file.name, priority
            );
            self.api
                .set_file_priority(&hash, Sep::from_str(&file.index.to_string())?, priority)
                .await?;
        }

        println!("[pnp2p] download_selected: waiting 500ms for priorities to apply");
        sleep(Duration::from_millis(500)).await;

        println!("[pnp2p] download_selected: starting torrent");
        self.api.start_torrents(vec![hash.clone()]).await?;

        // Progress monitoring
        println!("[pnp2p] download_selected: entering progress monitoring loop");
        let mut last_printed = -1.0;
        loop {
            if let Some(ref cancelfile) = self.cfile {
                if try_exists(cancelfile).await.unwrap_or(false) {
                    println!("[pnp2p] download_selected: cancel file detected, deleting torrent");
                    self.api.delete_torrents(vec![hash.clone()], true).await?;
                    println!(
                        "{}",
                        lib_pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, leaf],
                            data = ["3", "CANCELFILE"]
                        )
                        .unwrap()
                    );
                    return Ok(());
                }
            }

            let torrents = self
                .api
                .get_torrent_list(GetTorrentListArg::builder().hashes(hash.clone()).build())
                .await?;
            let torrent = torrents.first().ok_or("Torrent not found")?;
            println!(
                "[pnp2p] download_selected: state={:?}, progress={}",
                torrent.state,
                torrent.progress.unwrap_or(0.0)
            );

            if let Some(State::Error) | Some(State::MissingFiles) = torrent.state {
                println!("[pnp2p] download_selected: error state, deleting torrent");
                self.api.delete_torrents(vec![hash.clone()], true).await?;
                println!(
                    "{}",
                    lib_pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, leaf],
                        data = ["2", "ERROR"]
                    )
                    .unwrap()
                );
                return Err("Torrent entered error state".into());
            }

            if download_finished(torrent) {
                println!(
                    "[pnp2p] download_selected: download complete (state={:?}), breaking",
                    torrent.state
                );
                break;
            }

            let percent = (torrent.progress.unwrap_or(0.0) * 100.0).ceil();
            if (percent - last_printed).abs() >= 0.1 {
                let props = self.api.get_torrent_properties(&hash).await?;
                let downloaded = (props.total_downloaded.unwrap_or(0) as f32).abs();
                let total = (props.total_size.unwrap_or(1) as f32).abs();
                println!(
                    "[pnp2p] download_selected: progress update: {}% ({}MB/{}MB)",
                    percent,
                    downloaded as f64 / 1_048_576.0,
                    total as f64 / 1_048_576.0
                );
                println!(
                    "{}",
                    lib_pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, [leaf, leaf, leaf]],
                        data = ["0", [percent, downloaded, total]]
                    )
                    .unwrap()
                );
                last_printed = percent;
            }
            sleep(Duration::from_secs(5)).await;
        }

        println!("[pnp2p] download_selected: deleting torrent (keep files = false)");
        self.api.delete_torrents(vec![hash], false).await?;
        sleep(Duration::from_millis(500)).await;
        println!("[pnp2p] download_selected: download complete, sending DONE");
        println!(
            "{}",
            lib_pn_emit!(
                protocol = proto,
                negkey = &neg,
                schema = [leaf, leaf],
                data = ["1", "DONE"]
            )
            .unwrap()
        );
        println!("[pnp2p] download_selected: success");
        Ok(())
    }

    pub async fn download_and_remove(
        &self,
        torrent_path: &str,
        save_path: &str,
        proto: Protocol,
        neg: String,
        srcmgn: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        println!(
            "[pnp2p] download_and_remove entry: torrent_path={}, save_path={}, srcmgn={}",
            torrent_path, save_path, srcmgn
        );
        let source = if srcmgn {
            println!("[pnp2p] download_and_remove: creating source from URL(s)");
            TorrentSource::Urls {
                urls: Sep::<reqwest::Url, '\n'>::from_str(torrent_path)?,
            }
        } else {
            println!("[pnp2p] download_and_remove: reading torrent file from disk");
            let torrent_bytes = fs::read(torrent_path).await?;
            TorrentSource::TorrentFiles {
                torrents: vec![TorrentFile {
                    filename: torrent_path.to_string(),
                    data: torrent_bytes,
                }],
            }
        };

        println!("[pnp2p] download_and_remove: adding torrent (not paused)");
        let add_args = AddTorrentArg::builder()
            .source(source)
            .savepath(host_save_path(save_path))
            .build();

        self.api.add_torrent(add_args).await?;
        println!("[pnp2p] download_and_remove: add_torrent succeeded, waiting 1s");
        sleep(Duration::from_secs(1)).await;

        println!("[pnp2p] download_and_remove: fetching torrent list to get hash");
        let hash = self
            .api
            .get_torrent_list(GetTorrentListArg::builder().build())
            .await?
            .iter()
            .max_by_key(|t| t.added_on.unwrap_or(0))
            .ok_or("No torrents found")?
            .hash
            .clone()
            .unwrap();
        println!("[pnp2p] download_and_remove: obtained hash = {}", hash);

        println!("[pnp2p] download_and_remove: entering progress monitoring loop");
        let mut last_printed: f64 = -1.0;

        loop {
            if let Some(ref cancelfile) = self.cfile {
                if try_exists(cancelfile).await.unwrap_or(false) {
                    println!("[pnp2p] download_and_remove: cancel file detected, deleting torrent");
                    self.api.delete_torrents(vec![hash.clone()], true).await?;
                    println!(
                        "{}",
                        lib_pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, leaf],
                            data = ["3", "CANCELFILE"]
                        )
                        .unwrap()
                    );
                    return Ok(());
                }
            }

            let torrents = self
                .api
                .get_torrent_list(GetTorrentListArg::builder().hashes(hash.clone()).build())
                .await?;
            let torrent = torrents.first().ok_or("Torrent not found")?;
            println!(
                "[pnp2p] download_and_remove: state={:?}, progress={}",
                torrent.state,
                torrent.progress.unwrap_or(0.0)
            );

            if let Some(State::Error) | Some(State::MissingFiles) = torrent.state {
                println!("[pnp2p] download_and_remove: error state, deleting torrent");
                self.api.delete_torrents(vec![hash.clone()], true).await?;
                println!(
                    "{}",
                    lib_pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, leaf],
                        data = ["2", "ERROR"]
                    )
                    .unwrap()
                );
                return Err("Torrent entered error state".into());
            }

            if download_finished(torrent) {
                println!(
                    "[pnp2p] download_and_remove: download complete (state={:?}), breaking",
                    torrent.state
                );
                break;
            }
            let percent = (torrent.progress.unwrap_or(0.0) * 100.0).ceil();
            if (percent - last_printed).abs() >= 0.1 {
                let props = self.api.get_torrent_properties(&hash).await?;
                let downloaded = (props.total_downloaded.unwrap_or(0) as f32).abs();
                let total = (props.total_size.unwrap_or(1) as f32).abs();
                println!(
                    "[pnp2p] download_and_remove: progress update: {}% ({}MB/{}MB)",
                    percent,
                    downloaded as f64 / 1_048_576.0,
                    total as f64 / 1_048_576.0
                );
                println!(
                    "{}",
                    lib_pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, [leaf, leaf, leaf]],
                        data = ["0", [percent, downloaded, total]]
                    )
                    .unwrap()
                );
                last_printed = percent;
            }

            sleep(Duration::from_secs(5)).await;
        }

        let torrents = self
            .api
            .get_torrent_list(GetTorrentListArg::builder().hashes(hash.clone()).build())
            .await?;
        if let Some(content_path) = torrents.first().and_then(|t| t.content_path.clone()) {
            copy_mkv_files_to_save_path(&PathBuf::from(content_path), Path::new(save_path)).await?;
        }

        println!("[pnp2p] download_and_remove: deleting torrent (keep files = false)");
        self.api.delete_torrents(vec![hash], false).await?;
        sleep(Duration::from_millis(500)).await;
        println!("[pnp2p] download_and_remove: download complete, sending DONE");
        println!(
            "{}",
            lib_pn_emit!(
                protocol = proto,
                negkey = &neg,
                schema = [leaf, leaf],
                data = ["1", "DONE"]
            )
            .unwrap()
        );
        println!("[pnp2p] download_and_remove: success");
        Ok(())
    }
}

fn download_finished(t: &Torrent) -> bool {
    if matches!(
        t.state,
        Some(State::CheckingUP)
            | Some(State::CheckingDL)
            | Some(State::CheckingResumeData)
            | Some(State::Moving)
    ) {
        return false;
    }
    if matches!(
        t.state,
        Some(State::Uploading)
            | Some(State::StalledUP)
            | Some(State::ForcedUP)
            | Some(State::QueuedUP)
            | Some(State::PausedUP)
    ) {
        return true;
    }
    t.completion_on.map_or(false, |c| c > 0)
}

fn host_save_path(container_path: &str) -> String {
    let host_prefix = match std::env::var("PNP2P_QBIT_SAVE_HOST") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return container_path.to_string(),
    };
    let container_prefix =
        std::env::var("PNP2P_QBIT_SAVE_CONTAINER").unwrap_or_else(|_| "/app/DB".to_string());
    let rest = container_path
        .strip_prefix(&container_prefix)
        .unwrap_or(container_path);
    let host_prefix = host_prefix.trim_end_matches(['/', '\\']);
    let windows = host_prefix.contains('\\') || host_prefix.as_bytes().get(1) == Some(&b':');
    let joined = format!("{}{}", host_prefix, rest);
    if windows {
        joined.replace('/', "\\")
    } else {
        joined
    }
}

async fn copy_mkv_files_to_save_path(
    source: &Path,
    save_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !source.exists() {
        return Ok(());
    }
    fs::create_dir_all(save_path).await?;
    let mut stack = vec![source.to_path_buf()];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            let mut entries = fs::read_dir(&path).await?;
            while let Some(entry) = entries.next_entry().await? {
                stack.push(entry.path());
            }
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .map(is_video_ext)
            .unwrap_or(false)
        {
            let target = save_path.join(path.file_name().unwrap_or_default());
            if path != target {
                fs::copy(&path, &target).await?;
            }
        }
    }
    Ok(())
}
