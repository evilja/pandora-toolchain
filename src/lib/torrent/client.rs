use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::sleep;

use super::error::{Result, TorrentError};
use super::metainfo::{Magnet, Metainfo, TorrentFile};
use super::peer::{
    CompletedPiece, MEMORY_UNIT, PeerSettings, PieceScheduler, download_from_peer, fetch_metadata,
};
use super::proxy::ProxyConfig;
use super::storage::Storage;
use super::tracker::TrackerClient;

const DEFAULT_CONNECTIONS: usize = 24;
const DEFAULT_PIPELINE: usize = 32;
const DEFAULT_CANDIDATES: usize = 256;
const DEFAULT_TRACKER_ROUNDS: usize = 3;
const MAX_TORRENT_FILE_SIZE: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub proxy: Option<ProxyConfig>,
    pub max_connections: usize,
    pub max_peer_candidates: usize,
    pub request_pipeline: usize,
    pub block_size: usize,
    pub listen_port: u16,
    pub connect_timeout: Duration,
    pub peer_timeout: Duration,
    pub tracker_timeout: Duration,
    pub metadata_size_limit: usize,
    pub memory_buffer_bytes: usize,
    pub tracker_rounds: usize,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            proxy: None,
            max_connections: DEFAULT_CONNECTIONS,
            max_peer_candidates: DEFAULT_CANDIDATES,
            request_pipeline: DEFAULT_PIPELINE,
            block_size: 16 * 1024,
            listen_port: 6881,
            connect_timeout: Duration::from_secs(10),
            peer_timeout: Duration::from_secs(35),
            tracker_timeout: Duration::from_secs(15),
            metadata_size_limit: 8 * 1024 * 1024,
            memory_buffer_bytes: 128 * 1024 * 1024,
            tracker_rounds: DEFAULT_TRACKER_ROUNDS,
        }
    }
}

impl ClientConfig {
    pub fn from_env() -> Result<Self> {
        let mut config = Self::default();
        config.proxy = ProxyConfig::from_env()?;
        config.max_connections =
            env_usize("PNP2P_MAX_CONNECTIONS", config.max_connections, 1, 256)?;
        config.max_peer_candidates = env_usize(
            "PNP2P_MAX_PEER_CANDIDATES",
            config.max_peer_candidates,
            config.max_connections,
            4096,
        )?;
        config.request_pipeline = env_usize("PNP2P_PIPELINE", config.request_pipeline, 1, 256)?;
        config.block_size = env_usize("PNP2P_BLOCK_SIZE", config.block_size, 1024, 16 * 1024)?;
        config.listen_port = env_u16("PNP2P_PORT", config.listen_port)?;
        config.metadata_size_limit = env_usize(
            "PNP2P_METADATA_LIMIT",
            config.metadata_size_limit,
            16 * 1024,
            64 * 1024 * 1024,
        )?;
        config.memory_buffer_bytes = env_usize(
            "PNP2P_MEMORY_BUFFER",
            config.memory_buffer_bytes,
            1024 * 1024,
            usize::try_from(4_294_967_296u64).unwrap_or(usize::MAX),
        )?;
        config.tracker_rounds = env_usize("PNP2P_TRACKER_ROUNDS", config.tracker_rounds, 1, 20)?;
        Ok(config)
    }
}

#[derive(Clone, Debug)]
pub enum TorrentSource {
    File(PathBuf),
    Bytes(Vec<u8>),
    Magnet(String),
}

impl TorrentSource {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }

    pub fn magnet(value: impl Into<String>) -> Self {
        Self::Magnet(value.into())
    }
}

#[derive(Clone, Debug, Default)]
pub enum FileSelection {
    #[default]
    All,
    Only(Vec<u64>),
}

#[derive(Clone, Debug, Default)]
pub struct DownloadOptions {
    pub selection: FileSelection,
    pub cancel_file: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub enum DownloadEvent {
    Metadata {
        name: String,
        files: Vec<TorrentFile>,
        total_bytes: u64,
    },
    FileSelected {
        index: u64,
        path: PathBuf,
        bytes: u64,
    },
    Progress {
        downloaded_bytes: u64,
        total_bytes: u64,
        percent: f64,
    },
    // The verified contiguous prefix of one file. Torrent storage is preallocated to its final
    // length, so this event — not file metadata — is what makes a growing prefix safe to decode.
    FilePrefix {
        index: u64,
        path: PathBuf,
        available_bytes: u64,
        total_bytes: u64,
    },
    FileComplete {
        index: u64,
        path: PathBuf,
        bytes: u64,
    },
    Complete,
}

// One selected file's share of the piece map. A batch download announces a file the moment its
// last piece lands instead of waiting for the whole torrent, which is what lets the encoder start
// on episode 1 while episode 12 is still transferring.
struct FileProgress {
    index: u64,
    path: PathBuf,
    offset: u64,
    length: u64,
    first_piece: usize,
    last_piece: usize,
    remaining: usize,
    next_prefix_piece: usize,
    prefix_bytes: u64,
    done: bool,
}

fn file_progress(metainfo: &Metainfo, selected: Option<&HashSet<u64>>) -> Vec<FileProgress> {
    metainfo
        .files
        .iter()
        .filter(|file| selected.is_none_or(|selection| selection.contains(&file.index)))
        .map(|file| {
            let (first_piece, last_piece, remaining) = if file.length == 0 {
                (0, 0, 0)
            } else {
                let first = (file.offset / metainfo.piece_length) as usize;
                let last = ((file.offset + file.length - 1) / metainfo.piece_length) as usize;
                (first, last, last - first + 1)
            };
            FileProgress {
                index: file.index,
                path: file.path.clone(),
                offset: file.offset,
                length: file.length,
                first_piece,
                last_piece,
                remaining,
                next_prefix_piece: first_piece,
                prefix_bytes: 0,
                done: file.length == 0,
            }
        })
        .collect()
}

fn advance_file_prefix(
    file: &mut FileProgress,
    metainfo: &Metainfo,
    written: &HashSet<usize>,
) -> Result<bool> {
    let before = file.prefix_bytes;
    while file.next_prefix_piece <= file.last_piece && written.contains(&file.next_prefix_piece) {
        let piece_end = ((file.next_prefix_piece as u64 + 1) * metainfo.piece_length)
            .min(metainfo.total_length);
        let file_end = file
            .offset
            .checked_add(file.length)
            .ok_or_else(|| TorrentError::metainfo("file prefix end overflow"))?;
        file.prefix_bytes = piece_end.min(file_end).saturating_sub(file.offset);
        file.next_prefix_piece += 1;
    }
    Ok(file.prefix_bytes != before)
}

// Writing a verified piece, charging it against every file it overlaps, and announcing the files
// it finished. Shared by the drain and the select arm of the download loop so both paths keep the
// same accounting.
#[allow(clippy::too_many_arguments)]
async fn write_completed_piece<F>(
    piece: CompletedPiece,
    metainfo: &Metainfo,
    storage: &mut Storage,
    files: &mut [FileProgress],
    written: &mut HashSet<usize>,
    written_pieces: &mut usize,
    downloaded_bytes: &mut u64,
    required_bytes: u64,
    event: &mut F,
) -> Result<()>
where
    F: FnMut(DownloadEvent),
{
    if !written.insert(piece.index) {
        return Ok(());
    }
    let offset = (piece.index as u64)
        .checked_mul(metainfo.piece_length)
        .ok_or_else(|| TorrentError::metainfo("piece write offset overflow"))?;
    storage.write_piece(offset, &piece.data).await?;
    *written_pieces += 1;
    *downloaded_bytes = downloaded_bytes
        .checked_add(piece.data.len() as u64)
        .ok_or_else(|| TorrentError::metainfo("download progress overflow"))?;
    let percent = if required_bytes == 0 {
        100.0
    } else {
        (*downloaded_bytes as f64 * 100.0 / required_bytes as f64).min(100.0)
    };
    event(DownloadEvent::Progress {
        downloaded_bytes: *downloaded_bytes,
        total_bytes: required_bytes,
        percent,
    });
    for file in files.iter_mut() {
        if file.done || piece.index < file.first_piece || piece.index > file.last_piece {
            continue;
        }
        file.remaining = file.remaining.saturating_sub(1);
        if advance_file_prefix(file, metainfo, written)? {
            event(DownloadEvent::FilePrefix {
                index: file.index,
                path: file.path.clone(),
                available_bytes: file.prefix_bytes,
                total_bytes: file.length,
            });
        }
        if file.remaining > 0 {
            continue;
        }
        file.done = true;
        storage.flush_file(file.index).await?;
        event(DownloadEvent::FileComplete {
            index: file.index,
            path: file.path.clone(),
            bytes: file.length,
        });
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct DownloadSummary {
    pub info_hash: [u8; 20],
    pub files: Vec<TorrentFile>,
    pub downloaded_bytes: u64,
}

pub struct TorrentClient {
    config: ClientConfig,
    peer_id: [u8; 20],
    trackers: TrackerClient,
}

impl TorrentClient {
    pub fn new(config: ClientConfig) -> Result<Self> {
        let peer_id = generate_peer_id()?;
        let trackers = TrackerClient::new(
            config.proxy.clone(),
            config.tracker_timeout,
            peer_id,
            config.listen_port,
        )?;
        Ok(Self {
            config,
            peer_id,
            trackers,
        })
    }

    pub fn from_env() -> Result<Self> {
        Self::new(ClientConfig::from_env()?)
    }

    pub async fn metadata(&self, source: &TorrentSource) -> Result<Metainfo> {
        let (sender, receiver) = watch::channel(false);
        let _keep_sender_alive = sender;
        self.resolve_source(source, receiver)
            .await
            .map(|(meta, _)| meta)
    }

    pub async fn probe(&self, source: &TorrentSource) -> Result<Vec<TorrentFile>> {
        Ok(self.metadata(source).await?.files)
    }

    pub async fn download<F>(
        &self,
        source: &TorrentSource,
        destination: impl AsRef<Path>,
        options: DownloadOptions,
        mut event: F,
    ) -> Result<DownloadSummary>
    where
        F: FnMut(DownloadEvent),
    {
        let (cancel_sender, cancel_receiver, cancel_monitor) =
            cancellation_channel(options.cancel_file.clone()).await;
        if *cancel_receiver.borrow() {
            return Err(TorrentError::Cancelled);
        }
        let result = self
            .download_inner(
                source,
                destination.as_ref(),
                options.selection,
                &mut event,
                cancel_receiver,
            )
            .await;
        drop(cancel_sender);
        if let Some(monitor) = cancel_monitor {
            monitor.abort();
        }
        result
    }

    async fn download_inner<F>(
        &self,
        source: &TorrentSource,
        destination: &Path,
        selection: FileSelection,
        event: &mut F,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<DownloadSummary>
    where
        F: FnMut(DownloadEvent),
    {
        let (metainfo, initial_peers) = self.resolve_source(source, cancel.clone()).await?;
        event(DownloadEvent::Metadata {
            name: metainfo.name.clone(),
            files: metainfo.files.clone(),
            total_bytes: metainfo.total_length,
        });
        let selected = selection_set(selection);
        if let Some(selected) = selected.as_ref() {
            for index in selected {
                let file = metainfo
                    .files
                    .iter()
                    .find(|file| file.index == *index)
                    .ok_or_else(|| {
                        TorrentError::metainfo(format!(
                            "selected file index {index} does not exist"
                        ))
                    })?;
                event(DownloadEvent::FileSelected {
                    index: file.index,
                    path: file.path.clone(),
                    bytes: file.length,
                });
            }
        }
        let required = metainfo.selected_pieces(selected.as_ref())?;
        let required_bytes = required
            .iter()
            .enumerate()
            .filter(|(_, required)| **required)
            .try_fold(0u64, |total, (index, _)| {
                let size = metainfo
                    .piece_size(index)
                    .ok_or_else(|| TorrentError::metainfo("required piece is missing"))?;
                total
                    .checked_add(size as u64)
                    .ok_or_else(|| TorrentError::metainfo("selected byte count overflow"))
            })?;
        let mut storage = Storage::create(destination, &metainfo, selected.as_ref()).await?;
        let mut files = file_progress(&metainfo, selected.as_ref());
        let scheduler = Arc::new(PieceScheduler::new(required));
        if scheduler.required_count() == 0 {
            storage.finish().await?;
            event(DownloadEvent::Complete);
            return Ok(DownloadSummary {
                info_hash: metainfo.info_hash,
                files: metainfo
                    .files
                    .iter()
                    .filter(|file| {
                        selected
                            .as_ref()
                            .is_none_or(|selection| selection.contains(&file.index))
                    })
                    .cloned()
                    .collect(),
                downloaded_bytes: 0,
            });
        }

        let metainfo = Arc::new(metainfo);
        let settings = self.peer_settings();
        let semaphore = Arc::new(Semaphore::new(self.config.max_connections));
        let piece_units = metainfo
            .piece_length
            .try_into()
            .ok()
            .map(|length: usize| length.div_ceil(MEMORY_UNIT))
            .unwrap_or(1)
            .max(1);
        let configured_units = self.config.memory_buffer_bytes.div_ceil(MEMORY_UNIT);
        let memory = Arc::new(Semaphore::new(configured_units.max(piece_units)));
        let (piece_sender, mut piece_receiver) =
            mpsc::channel::<CompletedPiece>(self.config.max_connections.max(1));
        let mut tasks = JoinSet::new();
        let mut tried = HashSet::new();
        let mut tracker_round = 0usize;
        let mut candidate_errors = Vec::new();
        spawn_peer_tasks(
            &mut tasks,
            initial_peers,
            &mut tried,
            self.config.max_peer_candidates,
            semaphore.clone(),
            metainfo.clone(),
            self.peer_id,
            self.config.proxy.clone(),
            settings.clone(),
            scheduler.clone(),
            memory.clone(),
            piece_sender.clone(),
            cancel.clone(),
        );
        let mut written_pieces = 0usize;
        let mut downloaded_bytes = 0u64;
        let mut written = HashSet::new();
        for file in files.iter().filter(|file| file.done) {
            event(DownloadEvent::FileComplete {
                index: file.index,
                path: file.path.clone(),
                bytes: file.length,
            });
        }

        while written_pieces < scheduler.required_count() {
            if *cancel.borrow() {
                tasks.abort_all();
                return Err(TorrentError::Cancelled);
            }
            if tasks.is_empty() {
                while let Ok(piece) = piece_receiver.try_recv() {
                    write_completed_piece(
                        piece,
                        &metainfo,
                        &mut storage,
                        &mut files,
                        &mut written,
                        &mut written_pieces,
                        &mut downloaded_bytes,
                        required_bytes,
                        event,
                    )
                    .await?;
                }
                if written_pieces >= scheduler.required_count() {
                    break;
                }
                if tracker_round >= self.config.tracker_rounds {
                    let detail = if candidate_errors.is_empty() {
                        "all discovered peers were exhausted".to_string()
                    } else {
                        candidate_errors
                            .iter()
                            .rev()
                            .take(5)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("; ")
                    };
                    return Err(TorrentError::peer(format!(
                        "download stopped after writing {written_pieces}/{} pieces: {detail}",
                        scheduler.required_count()
                    )));
                }
                if tracker_round > 0 {
                    sleep(Duration::from_secs(
                        2u64.saturating_pow(tracker_round as u32).min(30),
                    ))
                    .await;
                }
                tracker_round += 1;
                match self
                    .trackers
                    .announce_all(
                        &metainfo.trackers,
                        metainfo.info_hash,
                        metainfo.selected_length(selected.as_ref()),
                    )
                    .await
                {
                    Ok(peers) => spawn_peer_tasks(
                        &mut tasks,
                        peers,
                        &mut tried,
                        self.config.max_peer_candidates,
                        semaphore.clone(),
                        metainfo.clone(),
                        self.peer_id,
                        self.config.proxy.clone(),
                        settings.clone(),
                        scheduler.clone(),
                        memory.clone(),
                        piece_sender.clone(),
                        cancel.clone(),
                    ),
                    Err(error) => candidate_errors.push(error.to_string()),
                }
                if tasks.is_empty() && tracker_round < self.config.tracker_rounds {
                    continue;
                }
                if tasks.is_empty() {
                    return Err(if tried.is_empty() {
                        TorrentError::NoPeers
                    } else {
                        TorrentError::peer("trackers returned no new usable peers")
                    });
                }
            }

            tokio::select! {
                piece = piece_receiver.recv() => {
                    let Some(piece) = piece else {
                        return Err(TorrentError::peer("all piece workers stopped unexpectedly"));
                    };
                    write_completed_piece(
                        piece,
                        &metainfo,
                        &mut storage,
                        &mut files,
                        &mut written,
                        &mut written_pieces,
                        &mut downloaded_bytes,
                        required_bytes,
                        event,
                    )
                    .await?;
                }
                task = tasks.join_next(), if !tasks.is_empty() => {
                    match task {
                        Some(Ok(Ok(()))) => {}
                        Some(Ok(Err(TorrentError::Cancelled))) if *cancel.borrow() => {
                            tasks.abort_all();
                            return Err(TorrentError::Cancelled);
                        }
                        Some(Ok(Err(error))) => candidate_errors.push(error.to_string()),
                        Some(Err(error)) => candidate_errors.push(error.to_string()),
                        None => {}
                    }
                }
                changed = cancel.changed() => {
                    if changed.is_ok() && *cancel.borrow() {
                        tasks.abort_all();
                        return Err(TorrentError::Cancelled);
                    }
                }
            }
        }
        tasks.abort_all();
        storage.finish().await?;
        event(DownloadEvent::Complete);
        let files = metainfo
            .files
            .iter()
            .filter(|file| {
                selected
                    .as_ref()
                    .is_none_or(|selection| selection.contains(&file.index))
            })
            .cloned()
            .collect();
        Ok(DownloadSummary {
            info_hash: metainfo.info_hash,
            files,
            downloaded_bytes,
        })
    }

    async fn resolve_source(
        &self,
        source: &TorrentSource,
        cancel: watch::Receiver<bool>,
    ) -> Result<(Metainfo, Vec<SocketAddr>)> {
        match source {
            TorrentSource::File(path) => {
                let metadata = tokio::fs::metadata(path).await?;
                if metadata.len() > MAX_TORRENT_FILE_SIZE as u64 {
                    return Err(TorrentError::metainfo(
                        "torrent file exceeds the 64 MiB limit",
                    ));
                }
                let bytes = tokio::fs::read(path).await?;
                if bytes.len() > MAX_TORRENT_FILE_SIZE {
                    return Err(TorrentError::metainfo(
                        "torrent file exceeds the 64 MiB limit",
                    ));
                }
                Ok((Metainfo::from_torrent_bytes(&bytes)?, Vec::new()))
            }
            TorrentSource::Bytes(bytes) => {
                if bytes.len() > MAX_TORRENT_FILE_SIZE {
                    return Err(TorrentError::metainfo(
                        "torrent bytes exceed the 64 MiB limit",
                    ));
                }
                Ok((Metainfo::from_torrent_bytes(bytes)?, Vec::new()))
            }
            TorrentSource::Magnet(value) => self.resolve_magnet(value, cancel).await,
        }
    }

    async fn resolve_magnet(
        &self,
        value: &str,
        cancel: watch::Receiver<bool>,
    ) -> Result<(Metainfo, Vec<SocketAddr>)> {
        let magnet = Magnet::parse(value)?;
        let mut errors = Vec::new();
        for round in 0..self.config.tracker_rounds {
            if *cancel.borrow() {
                return Err(TorrentError::Cancelled);
            }
            if round > 0 {
                sleep(Duration::from_secs(
                    2u64.saturating_pow(round as u32).min(30),
                ))
                .await;
            }
            let mut peers = match self
                .trackers
                .announce_all(&magnet.trackers, magnet.info_hash, 1)
                .await
            {
                Ok(peers) => peers,
                Err(error) => {
                    errors.push(error.to_string());
                    continue;
                }
            };
            if peers.is_empty() {
                errors.push("trackers returned no peers".to_string());
                continue;
            }
            match self
                .metadata_from_peers(&magnet, &peers, cancel.clone())
                .await
            {
                Ok((metadata, metadata_peer)) => {
                    prioritize_peer(&mut peers, metadata_peer);
                    return Ok((metadata, peers));
                }
                Err(error) => errors.push(error.to_string()),
            }
        }
        if *cancel.borrow() {
            Err(TorrentError::Cancelled)
        } else if errors.is_empty() {
            Err(TorrentError::NoPeers)
        } else {
            Err(TorrentError::peer(format!(
                "magnet metadata could not be fetched: {}",
                errors.join("; ")
            )))
        }
    }

    async fn metadata_from_peers(
        &self,
        magnet: &Magnet,
        peers: &[SocketAddr],
        cancel: watch::Receiver<bool>,
    ) -> Result<(Metainfo, SocketAddr)> {
        let semaphore = Arc::new(Semaphore::new(self.config.max_connections.min(16).max(1)));
        let mut tasks = JoinSet::new();
        for address in peers
            .iter()
            .copied()
            .filter(usable_peer)
            .take(self.config.max_peer_candidates.min(128))
        {
            let semaphore = semaphore.clone();
            let proxy = self.config.proxy.clone();
            let settings = self.peer_settings();
            let cancel = cancel.clone();
            let info_hash = magnet.info_hash;
            let peer_id = self.peer_id;
            tasks.spawn(async move {
                let result = match semaphore.acquire_owned().await {
                    Ok(_permit) => {
                        fetch_metadata(address, info_hash, peer_id, proxy, settings, cancel).await
                    }
                    Err(_) => Err(TorrentError::peer("metadata semaphore closed")),
                };
                (address, result)
            });
        }
        let mut errors = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok((address, Ok(info_bytes))) => {
                    tasks.abort_all();
                    let metadata = Metainfo::from_info_bytes(
                        &info_bytes,
                        magnet.trackers.clone(),
                        Some(magnet.info_hash),
                    )?;
                    return Ok((metadata, address));
                }
                Ok((_, Err(TorrentError::Cancelled))) if *cancel.borrow() => {
                    tasks.abort_all();
                    return Err(TorrentError::Cancelled);
                }
                Ok((_, Err(error))) => errors.push(error.to_string()),
                Err(error) => errors.push(error.to_string()),
            }
        }
        Err(if errors.is_empty() {
            TorrentError::NoPeers
        } else {
            TorrentError::peer(
                errors
                    .into_iter()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })
    }

    fn peer_settings(&self) -> PeerSettings {
        PeerSettings {
            connect_timeout: self.config.connect_timeout,
            read_timeout: self.config.peer_timeout,
            block_size: self.config.block_size,
            pipeline: self.config.request_pipeline,
            max_metadata_size: self.config.metadata_size_limit,
        }
    }
}

// The peer that supplied valid magnet metadata gets the first download connection attempt.
fn prioritize_peer(peers: &mut Vec<SocketAddr>, preferred: SocketAddr) {
    peers.retain(|peer| *peer != preferred);
    peers.insert(0, preferred);
}

#[allow(clippy::too_many_arguments)]
fn spawn_peer_tasks(
    tasks: &mut JoinSet<Result<()>>,
    peers: Vec<SocketAddr>,
    tried: &mut HashSet<SocketAddr>,
    max_candidates: usize,
    semaphore: Arc<Semaphore>,
    metainfo: Arc<Metainfo>,
    peer_id: [u8; 20],
    proxy: Option<ProxyConfig>,
    settings: PeerSettings,
    scheduler: Arc<PieceScheduler>,
    memory: Arc<Semaphore>,
    completed: mpsc::Sender<CompletedPiece>,
    cancel: watch::Receiver<bool>,
) {
    let remaining = max_candidates.saturating_sub(tried.len());
    for address in peers
        .into_iter()
        .filter(usable_peer)
        .filter(|address| tried.insert(*address))
        .take(remaining)
    {
        let semaphore = semaphore.clone();
        let metainfo = metainfo.clone();
        let proxy = proxy.clone();
        let settings = settings.clone();
        let scheduler = scheduler.clone();
        let memory = memory.clone();
        let completed = completed.clone();
        let cancel = cancel.clone();
        tasks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .map_err(|_| TorrentError::peer("peer semaphore closed"))?;
            download_from_peer(
                address, metainfo, peer_id, proxy, settings, scheduler, memory, completed, cancel,
            )
            .await
        });
    }
}

async fn cancellation_channel(
    path: Option<PathBuf>,
) -> (
    watch::Sender<bool>,
    watch::Receiver<bool>,
    Option<JoinHandle<()>>,
) {
    let initially_cancelled = match path.as_ref() {
        Some(path) => tokio::fs::try_exists(path).await.unwrap_or(false),
        None => false,
    };
    let (sender, receiver) = watch::channel(initially_cancelled);
    let monitor = path.map(|path| {
        let sender = sender.clone();
        tokio::spawn(async move {
            loop {
                if tokio::fs::try_exists(&path).await.unwrap_or(false) {
                    sender.send(true).ok();
                    return;
                }
                sleep(Duration::from_millis(250)).await;
            }
        })
    });
    (sender, receiver, monitor)
}

fn selection_set(selection: FileSelection) -> Option<HashSet<u64>> {
    match selection {
        FileSelection::All => None,
        FileSelection::Only(indices) => Some(indices.into_iter().collect()),
    }
}

fn usable_peer(address: &SocketAddr) -> bool {
    if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
        return false;
    }
    match address.ip() {
        IpAddr::V4(ip) => !ip.is_broadcast(),
        IpAddr::V6(_) => true,
    }
}

fn generate_peer_id() -> Result<[u8; 20]> {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut random = [0u8; 12];
    getrandom::getrandom(&mut random)
        .map_err(|error| TorrentError::peer(format!("random number generation failed: {error}")))?;
    let mut peer_id = [0u8; 20];
    peer_id[..8].copy_from_slice(b"-PN3300-");
    for (target, random) in peer_id[8..].iter_mut().zip(random) {
        *target = ALPHABET[usize::from(random) % ALPHABET.len()];
    }
    Ok(peer_id)
}

fn env_usize(key: &str, default: usize, minimum: usize, maximum: usize) -> Result<usize> {
    let Ok(value) = std::env::var(key) else {
        return Ok(default);
    };
    let parsed = value.parse::<usize>().map_err(|_| {
        TorrentError::metainfo(format!(
            "{key} must be an integer between {minimum} and {maximum}"
        ))
    })?;
    if !(minimum..=maximum).contains(&parsed) {
        return Err(TorrentError::metainfo(format!(
            "{key} must be between {minimum} and {maximum}"
        )));
    }
    Ok(parsed)
}

fn env_u16(key: &str, default: u16) -> Result<u16> {
    let Ok(value) = std::env::var(key) else {
        return Ok(default);
    };
    value
        .parse::<u16>()
        .map_err(|_| TorrentError::metainfo(format!("{key} must be a valid TCP port")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    use sha1::{Digest, Sha1};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::lib::torrent::bencode::{Value, decode, encode};
    use crate::lib::torrent::metainfo::hex_hash;

    #[test]
    fn generates_bep20_sized_peer_ids() {
        let id = generate_peer_id().unwrap();
        assert_eq!(id.len(), 20);
        assert_eq!(&id[..8], b"-PN3300-");
    }

    #[test]
    fn selection_deduplicates_indices() {
        assert_eq!(
            selection_set(FileSelection::Only(vec![1, 1, 2])).unwrap(),
            HashSet::from([1, 2])
        );
    }

    // A batch hands each finished file to an encoder, so the piece span a file occupies — including
    // the piece it shares with its neighbour — has to be exact.
    #[test]
    fn file_progress_spans_every_piece_a_file_touches() {
        let info = b"d5:filesld6:lengthi3e4:pathl5:a.mkveed6:lengthi3e4:pathl5:b.mkveee4:name4:root12:piece lengthi4e6:pieces40:0000000000000000000011111111111111111111e";
        let meta = Metainfo::from_info_bytes(info, vec![], None).unwrap();
        let files = file_progress(&meta, None);
        assert_eq!(files.len(), 2);
        assert_eq!((files[0].first_piece, files[0].last_piece), (0, 0));
        assert_eq!(files[0].remaining, 1);
        // The second file starts inside piece 0 and ends in piece 1.
        assert_eq!((files[1].first_piece, files[1].last_piece), (0, 1));
        assert_eq!(files[1].remaining, 2);
    }

    #[test]
    fn file_prefix_waits_for_a_gap_before_advancing() {
        let info = b"d5:filesld6:lengthi3e4:pathl5:a.mkveed6:lengthi3e4:pathl5:b.mkveee4:name4:root12:piece lengthi4e6:pieces40:0000000000000000000011111111111111111111e";
        let meta = Metainfo::from_info_bytes(info, vec![], None).unwrap();
        let mut file = file_progress(&meta, None).remove(1);
        let mut written = HashSet::from([1]);
        assert!(!advance_file_prefix(&mut file, &meta, &written).unwrap());
        assert_eq!(file.prefix_bytes, 0);

        written.insert(0);
        assert!(advance_file_prefix(&mut file, &meta, &written).unwrap());
        assert_eq!(file.prefix_bytes, 3);
    }

    #[test]
    fn file_progress_only_tracks_the_selected_files() {
        let info = b"d5:filesld6:lengthi3e4:pathl5:a.mkveed6:lengthi3e4:pathl5:b.mkveee4:name4:root12:piece lengthi4e6:pieces40:0000000000000000000011111111111111111111e";
        let meta = Metainfo::from_info_bytes(info, vec![], None).unwrap();
        let selected = HashSet::from([1]);
        let files = file_progress(&meta, Some(&selected));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].index, 1);
    }

    #[test]
    fn filters_unusable_peer_addresses() {
        assert!(!usable_peer(&"0.0.0.0:1".parse().unwrap()));
        assert!(!usable_peer(&"127.0.0.1:0".parse().unwrap()));
        assert!(usable_peer(&"127.0.0.1:6881".parse().unwrap()));
    }

    #[test]
    fn successful_metadata_peer_is_prioritized_for_downloading() {
        let preferred = "127.0.0.1:6881".parse().unwrap();
        let other = "127.0.0.2:6881".parse().unwrap();
        let mut peers = vec![other, preferred];
        prioritize_peer(&mut peers, preferred);
        assert_eq!(peers, vec![preferred, other]);
    }

    #[tokio::test]
    async fn downloads_and_verifies_a_piece_from_a_tracker_peer() {
        let content = b"a locally served torrent piece".to_vec();
        let info = test_info(&content);
        let info_hash: [u8; 20] = Sha1::digest(&info).into();
        let peer_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_address = peer_listener.local_addr().unwrap();
        let peer_content = content.clone();
        let peer_task = tokio::spawn(async move {
            let (mut stream, _) = peer_listener.accept().await.unwrap();
            serve_piece_peer(&mut stream, info_hash, &peer_content).await;
        });
        let tracker_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tracker_url = format!("http://{}/announce", tracker_listener.local_addr().unwrap());
        let tracker_task = tokio::spawn(serve_tracker_once(tracker_listener, peer_address));
        let torrent = test_torrent(&info, &tracker_url);
        let destination = scratch_dir();
        let mut progress = Vec::new();
        let mut prefixes = Vec::new();
        let client = TorrentClient::new(test_config()).unwrap();
        let summary = client
            .download(
                &TorrentSource::Bytes(torrent),
                &destination,
                DownloadOptions::default(),
                |event| match event {
                    DownloadEvent::Progress { percent, .. } => progress.push(percent),
                    DownloadEvent::FilePrefix { available_bytes, total_bytes, .. } => {
                        prefixes.push((available_bytes, total_bytes));
                    }
                    _ => {}
                },
            )
            .await
            .unwrap();
        assert_eq!(summary.info_hash, info_hash);
        assert_eq!(
            tokio::fs::read(destination.join("test.mkv")).await.unwrap(),
            content
        );
        assert_eq!(progress.last().copied(), Some(100.0));
        assert_eq!(prefixes.last().copied(), Some((content.len() as u64, content.len() as u64)));
        peer_task.await.unwrap();
        tracker_task.await.unwrap();
        tokio::fs::remove_dir_all(destination).await.ok();
    }

    #[tokio::test]
    async fn fetches_magnet_metadata_with_bep9() {
        let content = b"metadata payload target".to_vec();
        let info = test_info(&content);
        let info_hash: [u8; 20] = Sha1::digest(&info).into();
        let peer_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_address = peer_listener.local_addr().unwrap();
        let peer_info = info.clone();
        let peer_task = tokio::spawn(async move {
            let (mut stream, _) = peer_listener.accept().await.unwrap();
            serve_metadata_peer(&mut stream, info_hash, &peer_info).await;
        });
        let tracker_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tracker_url = format!("http://{}/announce", tracker_listener.local_addr().unwrap());
        let tracker_task = tokio::spawn(serve_tracker_once(tracker_listener, peer_address));
        let mut magnet =
            reqwest::Url::parse(&format!("magnet:?xt=urn:btih:{}", hex_hash(&info_hash))).unwrap();
        magnet.query_pairs_mut().append_pair("tr", &tracker_url);
        let client = TorrentClient::new(test_config()).unwrap();
        let metadata = client
            .metadata(&TorrentSource::Magnet(magnet.to_string()))
            .await
            .unwrap();
        assert_eq!(metadata.info_hash, info_hash);
        assert_eq!(metadata.files[0].path, PathBuf::from("test.mkv"));
        peer_task.await.unwrap();
        tracker_task.await.unwrap();
    }

    fn test_config() -> ClientConfig {
        ClientConfig {
            max_connections: 2,
            max_peer_candidates: 8,
            request_pipeline: 2,
            connect_timeout: Duration::from_secs(2),
            peer_timeout: Duration::from_secs(2),
            tracker_timeout: Duration::from_secs(2),
            tracker_rounds: 1,
            ..ClientConfig::default()
        }
    }

    fn test_info(content: &[u8]) -> Vec<u8> {
        let piece_hash: [u8; 20] = Sha1::digest(content).into();
        let mut info = BTreeMap::new();
        info.insert(b"length".to_vec(), Value::Integer(content.len() as i64));
        info.insert(b"name".to_vec(), Value::Bytes(b"test.mkv".to_vec()));
        info.insert(
            b"piece length".to_vec(),
            Value::Integer(content.len() as i64),
        );
        info.insert(b"pieces".to_vec(), Value::Bytes(piece_hash.to_vec()));
        encode(&Value::Dictionary(info))
    }

    fn test_torrent(info: &[u8], tracker: &str) -> Vec<u8> {
        let mut root = BTreeMap::new();
        root.insert(
            b"announce".to_vec(),
            Value::Bytes(tracker.as_bytes().to_vec()),
        );
        root.insert(b"info".to_vec(), decode(info).unwrap());
        encode(&Value::Dictionary(root))
    }

    fn scratch_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "pandora-torrent-client-{}-{nonce}",
            std::process::id()
        ))
    }

    async fn serve_tracker_once(listener: TcpListener, peer: SocketAddr) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(String::from_utf8_lossy(&request).contains("info_hash="));
        let SocketAddr::V4(peer) = peer else {
            panic!("test peer must use IPv4");
        };
        let mut compact = peer.ip().octets().to_vec();
        compact.extend_from_slice(&peer.port().to_be_bytes());
        let mut response = BTreeMap::new();
        response.insert(b"interval".to_vec(), Value::Integer(60));
        response.insert(b"peers".to_vec(), Value::Bytes(compact));
        let body = encode(&Value::Dictionary(response));
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(&body).await.unwrap();
    }

    async fn serve_piece_peer(stream: &mut TcpStream, info_hash: [u8; 20], content: &[u8]) {
        serve_handshake(stream, info_hash, false).await;
        assert_eq!(read_wire(stream).await, vec![2]);
        write_wire(stream, &[5, 0x80]).await;
        write_wire(stream, &[1]).await;
        let request = read_wire(stream).await;
        assert_eq!(request.first(), Some(&6));
        let index = u32::from_be_bytes(request[1..5].try_into().unwrap());
        let begin = u32::from_be_bytes(request[5..9].try_into().unwrap()) as usize;
        let length = u32::from_be_bytes(request[9..13].try_into().unwrap()) as usize;
        let mut response = vec![7];
        response.extend_from_slice(&index.to_be_bytes());
        response.extend_from_slice(&(begin as u32).to_be_bytes());
        response.extend_from_slice(&content[begin..begin + length]);
        write_wire(stream, &response).await;
    }

    async fn serve_metadata_peer(stream: &mut TcpStream, info_hash: [u8; 20], info: &[u8]) {
        serve_handshake(stream, info_hash, true).await;
        let handshake = read_wire(stream).await;
        assert_eq!(handshake.get(..2), Some(&[20, 0][..]));
        let mut extensions = BTreeMap::new();
        extensions.insert(b"ut_metadata".to_vec(), Value::Integer(3));
        let mut payload = BTreeMap::new();
        payload.insert(b"m".to_vec(), Value::Dictionary(extensions));
        payload.insert(b"metadata_size".to_vec(), Value::Integer(info.len() as i64));
        let mut response = vec![20, 0];
        response.extend_from_slice(&encode(&Value::Dictionary(payload)));
        write_wire(stream, &response).await;
        let request = read_wire(stream).await;
        assert_eq!(request.get(..2), Some(&[20, 3][..]));
        let mut header = BTreeMap::new();
        header.insert(b"msg_type".to_vec(), Value::Integer(1));
        header.insert(b"piece".to_vec(), Value::Integer(0));
        header.insert(b"total_size".to_vec(), Value::Integer(info.len() as i64));
        let mut response = vec![20, 1];
        response.extend_from_slice(&encode(&Value::Dictionary(header)));
        response.extend_from_slice(info);
        write_wire(stream, &response).await;
    }

    async fn serve_handshake(stream: &mut TcpStream, info_hash: [u8; 20], extensions: bool) {
        let mut request = [0u8; 68];
        stream.read_exact(&mut request).await.unwrap();
        assert_eq!(&request[28..48], &info_hash);
        let mut response = request;
        if extensions {
            response[25] |= 0x10;
        }
        response[48..68].copy_from_slice(b"-MOCK00-123456789012");
        stream.write_all(&response).await.unwrap();
    }

    async fn read_wire(stream: &mut TcpStream) -> Vec<u8> {
        let mut length = [0u8; 4];
        stream.read_exact(&mut length).await.unwrap();
        let mut payload = vec![0u8; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut payload).await.unwrap();
        payload
    }

    async fn write_wire(stream: &mut TcpStream, payload: &[u8]) {
        stream
            .write_all(&(payload.len() as u32).to_be_bytes())
            .await
            .unwrap();
        stream.write_all(payload).await.unwrap();
    }
}
