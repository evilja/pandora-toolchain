use std::path::PathBuf;
use std::str::FromStr;
use qbit_rs::Qbit;
use qbit_rs::model::{
    AddTorrentArg, Credential, GetTorrentListArg, Sep, State, TorrentFile, TorrentSource
};
use qbit_rs::model::Priority;
use tokio::time::{sleep, Duration};
use tokio::fs::{self, try_exists};
use crate::{lib_pn_data, lib_pn_emit, lib_pn_schema};
use crate::libpnprotocol::core::{Protocol, Schema};

pub struct P2p {
    api: Qbit,
    cfile: Option<PathBuf>,
}

impl P2p {
    pub async fn new(uname: &str, pass: &str, cfile: Option<String>) -> Self {
        let credential = Credential::new(uname, pass);
        Self {
            api: Qbit::new("http://localhost:8089", credential),
            cfile: cfile.map(PathBuf::from),
        }
    }

    pub async fn probe_torrent(
        &self,
        torrent_path: &str,
        srcmgn: bool,
    ) -> Result<Vec<(u64, String, u64)>, Box<dyn std::error::Error>> {
        let source = if srcmgn {
            TorrentSource::Urls { urls: Sep::<reqwest::Url, '\n'>::from_str(torrent_path)? }
        } else {
            let torrent_bytes = fs::read(torrent_path).await?;
            TorrentSource::TorrentFiles {
                torrents: vec![TorrentFile {
                    filename: torrent_path.to_string(),
                    data: torrent_bytes,
                }],
            }
        };

        let add_args = AddTorrentArg::builder()
            .source(source)
            .paused("true".to_string())   // String, not bool
            .savepath("/dev/null".to_string())
            .build();

        self.api.add_torrent(add_args).await?;

        // Find the hash (most recently added)
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

        // Wait for metadata (poll until contents are available)
        let mut files = Vec::new();
        for _ in 0..30 {
            sleep(Duration::from_secs(1)).await;
            // Pass `None` for indexes to get all files
            let contents = self.api.get_torrent_contents(&hash, None).await?;
            if !contents.is_empty() {
                files = contents;
                break;
            }
        }
        if files.is_empty() {
            self.api.delete_torrents(vec![hash], true).await?;
            return Err("Torrent metadata could not be fetched".into());
        }

        // Filter .mkv files and collect (index, name, size)
        let mkv_files: Vec<(u64, String, u64)> = files
            .iter()
            .filter(|f| f.name.to_lowercase().ends_with(".mkv"))
            .map(|f| (f.index, f.name.clone(), f.size))
            .collect();

        self.api.delete_torrents(vec![hash], true).await?;
        Ok(mkv_files)
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
        let source = if srcmgn {
            TorrentSource::Urls { urls: Sep::<reqwest::Url, '\n'>::from_str(torrent_path)? }
        } else {
            let torrent_bytes = fs::read(torrent_path).await?;
            TorrentSource::TorrentFiles {
                torrents: vec![TorrentFile {
                    filename: torrent_path.to_string(),
                    data: torrent_bytes,
                }],
            }
        };

        // Add torrent paused
        let add_args = AddTorrentArg::builder()
            .source(source)
            .paused("true".to_string())
            .savepath(save_path.to_string())
            .build();

        self.api.add_torrent(add_args).await?;
        sleep(Duration::from_secs(1)).await;

        // Get hash (most recent)
        let torrents = self.api
            .get_torrent_list(GetTorrentListArg::builder().build())
            .await?;
        let hash = torrents
            .iter()
            .max_by_key(|t| t.added_on.unwrap_or(0))
            .ok_or("No torrents found")?
            .hash.clone().unwrap();

        // Wait for metadata and get file list
        let mut files = Vec::new();
        for _ in 0..30 {
            sleep(Duration::from_secs(1)).await;
            let contents = self.api.get_torrent_contents(&hash, None).await?;
            if !contents.is_empty() {
                files = contents;
                break;
            }
        }
        if files.is_empty() {
            self.api.delete_torrents(vec![hash.clone()], Some(true)).await?;
            return Err("Could not retrieve file list for priority setting".into());
        }

        // Build pipe‑separated indices for all files
        let all_indices: Vec<i64> = (0..files.len() as i64).collect();
        let indices_str = all_indices
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("|");

        // We'll set priorities in two steps because set_file_priority applies the same priority
        // to all given indices. So we first set all files to DoNotDownload, then set selected to Normal.
        self.api
            .set_file_priority(&hash, Sep::from_str(&indices_str)?, Priority::DoNotDownload)
            .await?;

        if !file_indices.is_empty() {
            let select_indices: Vec<i64> = file_indices.iter().map(|&i| i as i64).collect();
            let select_str = select_indices
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join("|");
            self.api
                .set_file_priority(&hash, Sep::from_str(&select_str)?, Priority::Normal)
                .await?;
        }

        // Start the torrent (equivalent to resume)
        self.api.start_torrents(vec![hash.clone()]).await?;

        // --- Progress monitoring (same as before, but progress reflects selected files) ---
        let mut last_printed = -1.0;
        loop {
            if let Some(ref cancelfile) = self.cfile {
                if try_exists(cancelfile).await.unwrap_or(false) {
                    self.api.delete_torrents(vec![hash.clone()], Some(true)).await?;
                    println!("{}", lib_pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, leaf],
                        data = ["3", "CANCELFILE"]
                    ).unwrap());
                    return Ok(());
                }
            }

            let torrents = self.api
                .get_torrent_list(GetTorrentListArg::builder().hashes(hash.clone()).build())
                .await?;
            let torrent = torrents.first().ok_or("Torrent not found")?;

            match torrent.state {
                Some(State::Uploading) | Some(State::StalledUP) | Some(State::ForcedUP) | Some(State::QueuedUP) => {
                    break;
                }
                Some(State::Error) | Some(State::MissingFiles) => {
                    self.api.delete_torrents(vec![hash.clone()], Some(true)).await?;
                    println!("{}", lib_pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, leaf],
                        data = ["2", "ERROR"]
                    ).unwrap());
                    return Err("Torrent entered error state".into());
                }
                _ => {}
            }

            let percent = (torrent.progress.unwrap_or(0.0) * 100.0).ceil();
            if (percent - last_printed).abs() >= 0.1 {
                let props = self.api.get_torrent_properties(&hash).await?;
                let downloaded = (props.total_downloaded.unwrap_or(0) as f32).abs();
                let total = (props.total_size.unwrap_or(1) as f32).abs();
                println!("{}", lib_pn_emit!(
                    protocol = proto,
                    negkey = &neg,
                    schema = [leaf, [leaf, leaf, leaf]],
                    data = ["0", [percent, downloaded, total]]
                ).unwrap());
                last_printed = percent;
            }
            sleep(Duration::from_secs(5)).await;
        }

        println!("{}", lib_pn_emit!(
            protocol = proto,
            negkey = &neg,
            schema = [leaf, leaf],
            data = ["1", "DONE"]
        ).unwrap());
        self.api.delete_torrents(vec![hash], Some(false)).await?;
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
        let source = if srcmgn {
            TorrentSource::Urls { urls: Sep::<reqwest::Url, '\n'>::from_str(torrent_path)? }
        } else {
            let torrent_bytes = fs::read(torrent_path).await?;
            TorrentSource::TorrentFiles {
                torrents: vec![TorrentFile {
                    filename: torrent_path.to_string(),
                    data: torrent_bytes,
                }],
            }
        };

        let add_args = AddTorrentArg::builder()
            .source(source)
            .savepath(save_path.to_string())
            .build();

        self.api.add_torrent(add_args).await?;
        sleep(Duration::from_secs(1)).await;

        // --- FETCH BY PATH INSTEAD OF RECENCY ---
        let torrents = self.api
            .get_torrent_list(GetTorrentListArg::builder().build())
            .await?;

        let hash = torrents
            .iter()
            .find(|t| {
                // We check if the content_path contains our designated save_path
                // and if it ends with .mkv
                t.content_path.as_ref().map_or(false, |p| {
                    p.contains(save_path) && p.ends_with(".mkv")
                })
            })
            .map(|t| t.hash.clone().unwrap())
            .ok_or("Could not find torrent at the specified save path")?;
        // ----------------------------------------

        let mut last_printed: f64 = -1.0;

        loop {
            if let Some(ref cancelfile) = self.cfile {
                if try_exists(cancelfile).await.unwrap_or(false) {
                    self.api.delete_torrents(vec![hash.clone()], true).await?;
                    println!("{}", lib_pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, leaf],
                        data   = ["3", "CANCELFILE"]
                    ).unwrap());
                    return Ok(());
                }
            }

            let torrents = self.api
                .get_torrent_list(GetTorrentListArg::builder().hashes(hash.clone()).build())
                .await?;
            let torrent = torrents.first().ok_or("Torrent not found")?;

            match torrent.state {
                Some(State::Uploading) | Some(State::StalledUP) | Some(State::ForcedUP) | Some(State::QueuedUP) => {
                    break;
                }
                Some(State::Error) | Some(State::MissingFiles) => {
                    self.api.delete_torrents(vec![hash.clone()], true).await?;
                    println!("{}", lib_pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, leaf],
                        data   = ["2", "ERROR"]
                    ).unwrap());
                    return Err("Torrent entered error state".into());
                }
                _ => {}
            }
            let percent = (torrent.progress.unwrap_or(0.0) * 100.0).ceil();
            if (percent - last_printed).abs() >= 0.1 {
                let props = self.api.get_torrent_properties(&hash).await?;
                let downloaded = (props.total_downloaded.unwrap_or(0) as f32).abs();
                let total = (props.total_size.unwrap_or(1) as f32).abs();
                println!("{}" , lib_pn_emit!(
                    protocol = proto,
                    negkey = &neg,
                    schema = [leaf, [leaf, leaf, leaf]],
                    data   = ["0", [percent, downloaded, total]]
                ).unwrap());
                last_printed = percent;
            }

            sleep(Duration::from_secs(5)).await;
        }

        println!("{}", lib_pn_emit!(
            protocol = proto,
            negkey = &neg,
            schema = [leaf, leaf],
            data   = ["1", "DONE"]
        ).unwrap());
        self.api.delete_torrents(vec![hash], false).await?;
        Ok(())
    }
}
