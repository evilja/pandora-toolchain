use crate::lib::protocol::core::{Protocol, Schema};
use crate::{lib_pn_data, lib_pn_emit, lib_pn_schema};
use qbit_rs::Qbit;
use qbit_rs::model::Priority;
use qbit_rs::model::{
    AddTorrentArg, Credential, GetTorrentListArg, Sep, State, Torrent, TorrentFile, TorrentSource,
};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tokio::fs::{self, try_exists};
use tokio::time::{Duration, sleep};

pub struct P2p {
    api: Qbit,
    cfile: Option<PathBuf>,
}

fn is_video_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mkv" | "mp4" | "m4v" | "mov" | "avi" | "webm" | "ts" | "m2ts"
    )
}

fn is_video_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(is_video_ext)
        .unwrap_or(false)
}

pub async fn cleanup_pandora_qbit() {
    let qbit_host =
        std::env::var("PNP2P_QBIT_HOST").unwrap_or_else(|_| "http://localhost:8089".to_string());
    let qbit_user = std::env::var("PNP2P_QBIT_USER").unwrap_or_else(|_| "admin".to_string());
    let qbit_pass = std::env::var("PNP2P_QBIT_PASS").unwrap_or_else(|_| "adminadmin".to_string());
    let p2p = P2p::new(&qbit_host, &qbit_user, &qbit_pass, None).await;
    let torrents = match p2p
        .api
        .get_torrent_list(GetTorrentListArg::builder().build())
        .await
    {
        Ok(torrents) => torrents,
        Err(e) => {
            eprintln!("[pnp2p] startup cleanup failed to list torrents: {e}");
            return;
        }
    };
    let hashes: Vec<String> = torrents
        .iter()
        .filter(|t| has_pandora_tag(t.tags.as_deref()))
        .filter_map(|t| t.hash.clone())
        .collect();
    if !hashes.is_empty() {
        if let Err(e) = p2p.api.delete_torrents(hashes, true).await {
            eprintln!("[pnp2p] startup cleanup failed to delete torrents: {e}");
        }
    }
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

    async fn fail_if_duplicate_hash(&self, hash: &str) -> Result<(), Box<dyn std::error::Error>> {
        let torrents = self
            .api
            .get_torrent_list(
                GetTorrentListArg::builder()
                    .hashes(hash.to_string())
                    .build(),
            )
            .await?;
        if let Some(torrent) = torrents.first() {
            return Err(duplicate_torrent_error(
                torrent.save_path.as_deref(),
                torrent.content_path.as_deref(),
            )
            .into());
        }
        Ok(())
    }

    async fn find_added_hash(
        &self,
        tag: Option<&str>,
        empty_msg: &'static str,
        expected_save_path: Option<&str>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        if let Some(tag) = tag {
            for _ in 0..30 {
                let torrents = self
                    .api
                    .get_torrent_list(GetTorrentListArg::builder().tag(tag.to_string()).build())
                    .await?;
                if let Some(torrent) = torrents.iter().max_by_key(|t| t.added_on.unwrap_or(0)) {
                    if has_other_pandora_tag(torrent.tags.as_deref(), tag) {
                        return Err(duplicate_torrent_error(
                            torrent.save_path.as_deref(),
                            torrent.content_path.as_deref(),
                        )
                        .into());
                    }
                    if let (Some(expected), Some(actual)) =
                        (expected_save_path, torrent.save_path.as_deref())
                    {
                        if normalize_qbit_path(expected) != normalize_qbit_path(actual) {
                            return Err(duplicate_torrent_error(
                                torrent.save_path.as_deref(),
                                torrent.content_path.as_deref(),
                            )
                            .into());
                        }
                    }
                    if let Some(hash) = torrent.hash.clone() {
                        return Ok(hash);
                    }
                }
                sleep(Duration::from_secs(1)).await;
            }
            return Err(duplicate_torrent_error(expected_save_path, None).into());
        }

        Ok(self
            .api
            .get_torrent_list(GetTorrentListArg::builder().build())
            .await?
            .iter()
            .max_by_key(|t| t.added_on.unwrap_or(0))
            .ok_or(empty_msg)?
            .hash
            .clone()
            .unwrap())
    }

    pub async fn probe_torrent(
        &self,
        torrent_path: &str,
        srcmgn: bool,
        tag: Option<String>,
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
        let save_path = temp_dir.to_str().unwrap().to_string();
        println!("[pnp2p] probe_torrent: creating temp dir {:?}", temp_dir);
        tokio::fs::create_dir_all(&temp_dir).await?;

        if let Some(hash) = source_info_hash(torrent_path, srcmgn).await? {
            self.fail_if_duplicate_hash(&hash).await?;
        }

        println!(
            "[pnp2p] probe_torrent: calling api.add_torrent with savepath {:?}",
            temp_dir
        );
        let mut add_args = AddTorrentArg::builder()
            .source(source)
            .paused("true".to_string())
            .savepath(save_path.clone())
            .build();
        add_args.tags = tag.clone();

        self.api.add_torrent(add_args).await?;
        println!("[pnp2p] probe_torrent: add_torrent succeeded");
        sleep(Duration::from_secs(1)).await;

        println!("[pnp2p] probe_torrent: fetching torrent list to get hash");
        let hash = self
            .find_added_hash(
                tag.as_deref(),
                "No torrent found after add",
                Some(&save_path),
            )
            .await?;
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
        proto: &Protocol,
        neg: String,
        srcmgn: bool,
        tag: Option<String>,
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

        if let Some(hash) = source_info_hash(torrent_path, srcmgn).await? {
            self.fail_if_duplicate_hash(&hash).await?;
        }

        println!("[pnp2p] download_selected: adding torrent paused");
        let qbit_save_path = host_save_path(save_path);
        let mut add_args = AddTorrentArg::builder()
            .source(source)
            .paused("true".to_string())
            .savepath(qbit_save_path.clone())
            .build();
        add_args.tags = tag.clone();

        self.api.add_torrent(add_args).await?;
        println!("[pnp2p] download_selected: add_torrent succeeded, waiting 1s");
        sleep(Duration::from_secs(1)).await;

        println!("[pnp2p] download_selected: fetching torrent list to get hash");
        let hash = self
            .find_added_hash(tag.as_deref(), "No torrents found", Some(&qbit_save_path))
            .await?;
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
        proto: &Protocol,
        neg: String,
        srcmgn: bool,
        tag: Option<String>,
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

        if let Some(hash) = source_info_hash(torrent_path, srcmgn).await? {
            self.fail_if_duplicate_hash(&hash).await?;
        }

        println!("[pnp2p] download_and_remove: adding torrent (not paused)");
        let qbit_save_path = host_save_path(save_path);
        let mut add_args = AddTorrentArg::builder()
            .source(source)
            .savepath(qbit_save_path.clone())
            .build();
        add_args.tags = tag.clone();

        self.api.add_torrent(add_args).await?;
        println!("[pnp2p] download_and_remove: add_torrent succeeded, waiting 1s");
        sleep(Duration::from_secs(1)).await;

        println!("[pnp2p] download_and_remove: fetching torrent list to get hash");
        let hash = self
            .find_added_hash(tag.as_deref(), "No torrents found", Some(&qbit_save_path))
            .await?;
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

async fn source_info_hash(
    torrent_path: &str,
    srcmgn: bool,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if srcmgn {
        return Ok(magnet_info_hash(torrent_path));
    }
    let data = fs::read(torrent_path).await?;
    Ok(torrent_info_hash(&data))
}

pub fn magnet_info_hash(magnet: &str) -> Option<String> {
    for part in magnet.split(['?', '&']) {
        if let Some(value) = part.strip_prefix("xt=urn:btih:") {
            if value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit()) {
                return Some(value.to_ascii_lowercase());
            }
        }
    }
    None
}

pub fn torrent_info_hash(data: &[u8]) -> Option<String> {
    let info_start = find_info_value_start(data)?;
    let info_end = bencode_value_end(data, info_start)?;
    let mut hasher = Sha1::new();
    hasher.update(&data[info_start..info_end]);
    Some(format!("{:x}", hasher.finalize()))
}

fn find_info_value_start(data: &[u8]) -> Option<usize> {
    if data.first().copied()? != b'd' {
        return None;
    }
    let mut pos = 1usize;
    while pos < data.len() && data[pos] != b'e' {
        let key_start = pos;
        let key_end = bencode_value_end(data, key_start)?;
        let key = bencode_bytes(data, key_start, key_end)?;
        pos = key_end;
        if key == b"info" {
            return Some(pos);
        }
        pos = bencode_value_end(data, pos)?;
    }
    None
}

fn bencode_bytes(data: &[u8], start: usize, end: usize) -> Option<&[u8]> {
    let colon = data[start..end].iter().position(|b| *b == b':')? + start;
    Some(&data[colon + 1..end])
}

fn bencode_value_end(data: &[u8], start: usize) -> Option<usize> {
    match *data.get(start)? {
        b'i' => {
            let rel = data[start..].iter().position(|b| *b == b'e')?;
            Some(start + rel + 1)
        }
        b'l' | b'd' => {
            let mut pos = start + 1;
            while *data.get(pos)? != b'e' {
                pos = bencode_value_end(data, pos)?;
            }
            Some(pos + 1)
        }
        b'0'..=b'9' => {
            let mut colon = start;
            while data.get(colon)?.is_ascii_digit() {
                colon += 1;
            }
            if *data.get(colon)? != b':' {
                return None;
            }
            let len = std::str::from_utf8(&data[start..colon])
                .ok()?
                .parse::<usize>()
                .ok()?;
            Some(colon + 1 + len)
        }
        _ => None,
    }
}

fn duplicate_torrent_error(save_path: Option<&str>, content_path: Option<&str>) -> String {
    format!(
        "DUPLICATE_TORRENT|{}",
        save_path.or(content_path).unwrap_or("")
    )
}

fn has_pandora_tag(tags: Option<&str>) -> bool {
    tags.unwrap_or("")
        .split(',')
        .map(|t| t.trim())
        .any(|t| t.starts_with("pandora-job-"))
}

fn has_other_pandora_tag(tags: Option<&str>, current: &str) -> bool {
    tags.unwrap_or("")
        .split(',')
        .map(|t| t.trim())
        .any(|t| t.starts_with("pandora-job-") && t != current)
}

fn normalize_qbit_path(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_string()
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
