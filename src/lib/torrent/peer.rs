use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::time::timeout;

use super::bencode::{Value, decode_prefix, encode};
use super::error::{Result, TorrentError};
use super::metainfo::Metainfo;
use super::proxy::{ProxyConfig, connect_peer};

const PROTOCOL: &[u8] = b"BitTorrent protocol";
const HANDSHAKE_LENGTH: usize = 68;
const MAX_WIRE_MESSAGE: usize = 2 * 1024 * 1024;
const METADATA_BLOCK: usize = 16 * 1024;
pub(crate) const MEMORY_UNIT: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct PeerSettings {
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub block_size: usize,
    pub pipeline: usize,
    pub max_metadata_size: usize,
}

pub(crate) struct CompletedPiece {
    pub index: usize,
    pub data: Vec<u8>,
    pub _memory_permit: OwnedSemaphorePermit,
}

struct ScheduleState {
    in_flight: Vec<bool>,
    complete: Vec<bool>,
    complete_count: usize,
}

pub(crate) struct PieceScheduler {
    required: Vec<bool>,
    required_count: usize,
    state: Mutex<ScheduleState>,
    changed: Notify,
}

impl PieceScheduler {
    pub(crate) fn new(required: Vec<bool>) -> Self {
        let required_count = required.iter().filter(|required| **required).count();
        let length = required.len();
        Self {
            required,
            required_count,
            state: Mutex::new(ScheduleState {
                in_flight: vec![false; length],
                complete: vec![false; length],
                complete_count: 0,
            }),
            changed: Notify::new(),
        }
    }

    fn claim(&self, available: &[bool]) -> Option<usize> {
        let mut state = self.state.lock().unwrap();
        for index in 0..self.required.len() {
            if self.required[index]
                && available.get(index).copied().unwrap_or(false)
                && !state.in_flight[index]
                && !state.complete[index]
            {
                state.in_flight[index] = true;
                return Some(index);
            }
        }
        None
    }

    fn release(&self, index: usize) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(in_flight) = state.in_flight.get_mut(index) {
                *in_flight = false;
            }
        }
        self.changed.notify_waiters();
    }

    fn complete(&self, index: usize) {
        let mut state = self.state.lock().unwrap();
        state.in_flight[index] = false;
        if !state.complete[index] {
            state.complete[index] = true;
            state.complete_count += 1;
        }
        drop(state);
        self.changed.notify_waiters();
    }

    fn has_compatible_piece(&self, available: &[bool]) -> bool {
        let state = self.state.lock().unwrap();
        self.required.iter().enumerate().any(|(index, required)| {
            *required && available.get(index).copied().unwrap_or(false) && !state.complete[index]
        })
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.state.lock().unwrap().complete_count == self.required_count
    }

    pub(crate) fn required_count(&self) -> usize {
        self.required_count
    }
}

pub(crate) async fn fetch_metadata(
    address: SocketAddr,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
    proxy: Option<ProxyConfig>,
    settings: PeerSettings,
    mut cancel: watch::Receiver<bool>,
) -> Result<Vec<u8>> {
    let mut stream = connect_peer(address, proxy.as_ref(), settings.connect_timeout).await?;
    handshake(
        &mut stream,
        info_hash,
        peer_id,
        true,
        settings.read_timeout,
        &mut cancel,
    )
    .await?;
    send_extended_handshake(&mut stream, settings.read_timeout, &mut cancel).await?;

    let mut remote_metadata_id = None;
    let mut metadata_size = None;
    while remote_metadata_id.is_none() || metadata_size.is_none() {
        let Some(message) = read_message(
            &mut stream,
            settings.read_timeout,
            &mut cancel,
            MAX_WIRE_MESSAGE,
        )
        .await?
        else {
            continue;
        };
        if message.first() != Some(&20) || message.get(1) != Some(&0) {
            continue;
        }
        let (handshake, _) = decode_prefix(&message[2..])?;
        remote_metadata_id = handshake
            .get(b"m")
            .and_then(|value| value.get(b"ut_metadata"))
            .and_then(Value::as_integer)
            .and_then(|value| u8::try_from(value).ok());
        metadata_size = handshake
            .get(b"metadata_size")
            .and_then(Value::as_integer)
            .and_then(|value| usize::try_from(value).ok());
    }
    let remote_metadata_id = remote_metadata_id.unwrap();
    if remote_metadata_id == 0 {
        return Err(TorrentError::peer(
            "peer advertised metadata extension id zero",
        ));
    }
    let metadata_size = metadata_size.unwrap();
    if metadata_size == 0 || metadata_size > settings.max_metadata_size {
        return Err(TorrentError::peer(format!(
            "peer advertised invalid metadata size {metadata_size}"
        )));
    }
    let piece_count = metadata_size.div_ceil(METADATA_BLOCK);
    let mut pieces: Vec<Option<Vec<u8>>> = vec![None; piece_count];
    let mut requested = 0usize;
    let mut received = 0usize;
    let metadata_pipeline = settings.pipeline.clamp(1, 16);
    while received < piece_count {
        while requested < piece_count && requested - received < metadata_pipeline {
            send_metadata_request(
                &mut stream,
                remote_metadata_id,
                requested,
                settings.read_timeout,
                &mut cancel,
            )
            .await?;
            requested += 1;
        }
        let Some(message) = read_message(
            &mut stream,
            settings.read_timeout,
            &mut cancel,
            MAX_WIRE_MESSAGE,
        )
        .await?
        else {
            continue;
        };
        if message.first() != Some(&20) || message.len() < 3 {
            continue;
        }
        let (header, consumed) = decode_prefix(&message[2..])?;
        let msg_type = header
            .get(b"msg_type")
            .and_then(Value::as_integer)
            .unwrap_or(-1);
        let piece = header
            .get(b"piece")
            .and_then(Value::as_integer)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| TorrentError::peer("metadata response has no piece index"))?;
        if msg_type == 2 {
            return Err(TorrentError::peer(format!(
                "peer rejected metadata piece {piece}"
            )));
        }
        if msg_type != 1 || piece >= piece_count || piece >= requested {
            continue;
        }
        let block = message
            .get(2 + consumed..)
            .ok_or_else(|| TorrentError::peer("truncated metadata response"))?;
        let expected = if piece + 1 == piece_count {
            metadata_size - piece * METADATA_BLOCK
        } else {
            METADATA_BLOCK
        };
        if block.len() != expected {
            return Err(TorrentError::peer(format!(
                "metadata piece {piece} has length {}, expected {expected}",
                block.len()
            )));
        }
        if pieces[piece].is_none() {
            pieces[piece] = Some(block.to_vec());
            received += 1;
        }
    }
    let mut metadata = Vec::with_capacity(metadata_size);
    for piece in pieces {
        metadata.extend_from_slice(
            &piece.ok_or_else(|| TorrentError::peer("metadata piece is missing"))?,
        );
    }
    let mut hasher = Sha1::new();
    hasher.update(&metadata);
    let actual: [u8; 20] = hasher.finalize().into();
    if actual != info_hash {
        return Err(TorrentError::peer(
            "peer supplied metadata with the wrong info hash",
        ));
    }
    Ok(metadata)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn download_from_peer(
    address: SocketAddr,
    metainfo: Arc<Metainfo>,
    peer_id: [u8; 20],
    proxy: Option<ProxyConfig>,
    settings: PeerSettings,
    scheduler: Arc<PieceScheduler>,
    memory: Arc<Semaphore>,
    completed: mpsc::Sender<CompletedPiece>,
    mut cancel: watch::Receiver<bool>,
) -> Result<()> {
    if scheduler.is_complete() {
        return Ok(());
    }
    let mut stream = connect_peer(address, proxy.as_ref(), settings.connect_timeout).await?;
    handshake(
        &mut stream,
        metainfo.info_hash,
        peer_id,
        false,
        settings.read_timeout,
        &mut cancel,
    )
    .await?;
    write_message(&mut stream, &[2], settings.read_timeout, &mut cancel).await?;

    let mut available = vec![false; metainfo.piece_hashes.len()];
    let mut saw_availability = false;
    let mut choked = true;
    while choked || !saw_availability {
        let Some(message) = read_message(
            &mut stream,
            settings.read_timeout,
            &mut cancel,
            MAX_WIRE_MESSAGE,
        )
        .await?
        else {
            continue;
        };
        process_control_message(&message, &mut available, &mut saw_availability, &mut choked)?;
    }

    loop {
        if *cancel.borrow() {
            return Err(TorrentError::Cancelled);
        }
        if scheduler.is_complete() {
            return Ok(());
        }
        if choked {
            let Some(message) = read_message(
                &mut stream,
                settings.read_timeout,
                &mut cancel,
                MAX_WIRE_MESSAGE,
            )
            .await?
            else {
                continue;
            };
            process_control_message(&message, &mut available, &mut saw_availability, &mut choked)?;
            continue;
        }
        let changed = scheduler.changed.notified();
        let Some(index) = scheduler.claim(&available) else {
            if scheduler.is_complete() {
                return Ok(());
            }
            if scheduler.has_compatible_piece(&available) {
                tokio::select! {
                    _ = timeout(Duration::from_secs(5), changed) => {}
                    result = cancel.changed() => {
                        if result.is_ok() && *cancel.borrow() {
                            return Err(TorrentError::Cancelled);
                        }
                    }
                }
            } else {
                let Some(message) = read_message(
                    &mut stream,
                    settings.read_timeout,
                    &mut cancel,
                    MAX_WIRE_MESSAGE,
                )
                .await?
                else {
                    continue;
                };
                process_control_message(
                    &message,
                    &mut available,
                    &mut saw_availability,
                    &mut choked,
                )?;
            }
            continue;
        };
        let piece_size = metainfo.piece_size(index).ok_or_else(|| {
            TorrentError::peer(format!("piece index {index} is outside the torrent"))
        })?;
        let permits = u32::try_from(piece_size.div_ceil(MEMORY_UNIT))
            .map_err(|_| TorrentError::peer("piece memory permit count overflow"))?;
        let acquire_memory = memory.clone().acquire_many_owned(permits);
        let memory_permit = tokio::select! {
            result = acquire_memory => result
                .map_err(|_| TorrentError::peer("piece memory budget closed"))?,
            result = cancel.changed() => {
                scheduler.release(index);
                if result.is_ok() && *cancel.borrow() {
                    return Err(TorrentError::Cancelled);
                }
                return Ok(());
            }
        };
        let piece = download_piece(
            &mut stream,
            index,
            piece_size,
            &settings,
            &mut available,
            &mut choked,
            &mut cancel,
        )
        .await;
        let piece = match piece {
            Ok(piece) => piece,
            Err(error) => {
                scheduler.release(index);
                return Err(error);
            }
        };
        let mut hasher = Sha1::new();
        hasher.update(&piece);
        let actual: [u8; 20] = hasher.finalize().into();
        if actual != metainfo.piece_hashes[index] {
            scheduler.release(index);
            return Err(TorrentError::peer(format!(
                "peer returned corrupt piece {index}"
            )));
        }
        let send = completed.send(CompletedPiece {
            index,
            data: piece,
            _memory_permit: memory_permit,
        });
        tokio::select! {
            result = send => {
                if result.is_err() {
                    scheduler.release(index);
                    return Ok(());
                }
            }
            changed = cancel.changed() => {
                scheduler.release(index);
                if changed.is_ok() && *cancel.borrow() {
                    return Err(TorrentError::Cancelled);
                }
                return Ok(());
            }
        }
        scheduler.complete(index);
    }
}

async fn download_piece(
    stream: &mut TcpStream,
    index: usize,
    piece_size: usize,
    settings: &PeerSettings,
    available: &mut [bool],
    choked: &mut bool,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Vec<u8>> {
    let block_size = settings.block_size.clamp(1024, 16 * 1024);
    let block_count = piece_size.div_ceil(block_size);
    let mut data = vec![0u8; piece_size];
    let mut received = vec![false; block_count];
    let mut outstanding = HashSet::new();
    let mut next_block = 0usize;
    let mut received_count = 0usize;

    while received_count < block_count {
        while !*choked && next_block < block_count && outstanding.len() < settings.pipeline.max(1) {
            let begin = next_block * block_size;
            let length = (piece_size - begin).min(block_size);
            send_request(stream, index, begin, length, settings.read_timeout, cancel).await?;
            outstanding.insert(begin);
            next_block += 1;
        }
        let Some(message) =
            read_message(stream, settings.read_timeout, cancel, MAX_WIRE_MESSAGE).await?
        else {
            continue;
        };
        match message.first().copied() {
            Some(0) => {
                *choked = true;
                return Err(TorrentError::peer("peer choked an in-flight piece"));
            }
            Some(1) => *choked = false,
            Some(4) => update_have(&message, available)?,
            Some(5) => update_bitfield(&message, available),
            Some(7) => {
                if message.len() < 9 {
                    return Err(TorrentError::peer("truncated piece message"));
                }
                let returned_index = u32::from_be_bytes(message[1..5].try_into().unwrap()) as usize;
                let begin = u32::from_be_bytes(message[5..9].try_into().unwrap()) as usize;
                if returned_index != index || !outstanding.remove(&begin) {
                    continue;
                }
                let block = &message[9..];
                if begin >= piece_size || block.len() != (piece_size - begin).min(block_size) {
                    return Err(TorrentError::peer("peer returned an invalid piece block"));
                }
                let block_index = begin / block_size;
                if !received[block_index] {
                    data[begin..begin + block.len()].copy_from_slice(block);
                    received[block_index] = true;
                    received_count += 1;
                }
            }
            _ => {}
        }
    }
    Ok(data)
}

fn process_control_message(
    message: &[u8],
    available: &mut [bool],
    saw_availability: &mut bool,
    choked: &mut bool,
) -> Result<()> {
    match message.first().copied() {
        Some(0) => *choked = true,
        Some(1) => *choked = false,
        Some(4) => {
            update_have(message, available)?;
            *saw_availability = true;
        }
        Some(5) => {
            update_bitfield(message, available);
            *saw_availability = true;
        }
        _ => {}
    }
    Ok(())
}

fn update_have(message: &[u8], available: &mut [bool]) -> Result<()> {
    if message.len() != 5 {
        return Err(TorrentError::peer("invalid have message"));
    }
    let index = u32::from_be_bytes(message[1..5].try_into().unwrap()) as usize;
    if let Some(piece) = available.get_mut(index) {
        *piece = true;
    }
    Ok(())
}

fn update_bitfield(message: &[u8], available: &mut [bool]) {
    for (index, target) in available.iter_mut().enumerate() {
        let byte = message.get(1 + index / 8).copied().unwrap_or(0);
        *target = byte & (0x80 >> (index % 8)) != 0;
    }
}

async fn handshake(
    stream: &mut TcpStream,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
    extensions: bool,
    timeout_duration: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
    let mut handshake = [0u8; HANDSHAKE_LENGTH];
    handshake[0] = PROTOCOL.len() as u8;
    handshake[1..20].copy_from_slice(PROTOCOL);
    if extensions {
        handshake[25] |= 0x10;
    }
    handshake[28..48].copy_from_slice(&info_hash);
    handshake[48..68].copy_from_slice(&peer_id);
    write_all_cancel(stream, &handshake, timeout_duration, cancel).await?;
    let mut response = [0u8; HANDSHAKE_LENGTH];
    read_exact_cancel(stream, &mut response, timeout_duration, cancel).await?;
    if response[0] != 19 || &response[1..20] != PROTOCOL {
        return Err(TorrentError::peer("peer returned an invalid handshake"));
    }
    if response[28..48] != info_hash {
        return Err(TorrentError::peer("peer handshake info hash differs"));
    }
    if response[48..68] == peer_id {
        return Err(TorrentError::peer("connected to our own peer id"));
    }
    if extensions && response[25] & 0x10 == 0 {
        return Err(TorrentError::peer(
            "peer does not support extension protocol metadata",
        ));
    }
    Ok(())
}

async fn send_extended_handshake(
    stream: &mut TcpStream,
    timeout_duration: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
    let mut extensions = BTreeMap::new();
    extensions.insert(b"ut_metadata".to_vec(), Value::Integer(1));
    let mut dictionary = BTreeMap::new();
    dictionary.insert(b"m".to_vec(), Value::Dictionary(extensions));
    dictionary.insert(b"p".to_vec(), Value::Integer(0));
    dictionary.insert(b"v".to_vec(), Value::Bytes(b"PandoraTorrent/1.0".to_vec()));
    let mut message = vec![20, 0];
    message.extend_from_slice(&encode(&Value::Dictionary(dictionary)));
    write_message(stream, &message, timeout_duration, cancel).await
}

async fn send_metadata_request(
    stream: &mut TcpStream,
    extension_id: u8,
    piece: usize,
    timeout_duration: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
    let mut dictionary = BTreeMap::new();
    dictionary.insert(b"msg_type".to_vec(), Value::Integer(0));
    dictionary.insert(
        b"piece".to_vec(),
        Value::Integer(
            i64::try_from(piece)
                .map_err(|_| TorrentError::peer("metadata piece index overflow"))?,
        ),
    );
    let mut message = vec![20, extension_id];
    message.extend_from_slice(&encode(&Value::Dictionary(dictionary)));
    write_message(stream, &message, timeout_duration, cancel).await
}

async fn send_request(
    stream: &mut TcpStream,
    index: usize,
    begin: usize,
    length: usize,
    timeout_duration: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
    let index = u32::try_from(index).map_err(|_| TorrentError::peer("piece index overflow"))?;
    let begin = u32::try_from(begin).map_err(|_| TorrentError::peer("block offset overflow"))?;
    let length = u32::try_from(length).map_err(|_| TorrentError::peer("block length overflow"))?;
    let mut message = Vec::with_capacity(13);
    message.push(6);
    message.extend_from_slice(&index.to_be_bytes());
    message.extend_from_slice(&begin.to_be_bytes());
    message.extend_from_slice(&length.to_be_bytes());
    write_message(stream, &message, timeout_duration, cancel).await
}

async fn write_message(
    stream: &mut TcpStream,
    payload: &[u8],
    timeout_duration: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| TorrentError::peer("wire message is too large"))?;
    let mut message = Vec::with_capacity(4 + payload.len());
    message.extend_from_slice(&length.to_be_bytes());
    message.extend_from_slice(payload);
    write_all_cancel(stream, &message, timeout_duration, cancel).await
}

async fn read_message(
    stream: &mut TcpStream,
    timeout_duration: Duration,
    cancel: &mut watch::Receiver<bool>,
    max_length: usize,
) -> Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    read_exact_cancel(stream, &mut length, timeout_duration, cancel).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 {
        return Ok(None);
    }
    if length > max_length {
        return Err(TorrentError::peer(format!(
            "wire message length {length} exceeds the limit"
        )));
    }
    let mut payload = vec![0u8; length];
    read_exact_cancel(stream, &mut payload, timeout_duration, cancel).await?;
    Ok(Some(payload))
}

async fn write_all_cancel(
    stream: &mut TcpStream,
    data: &[u8],
    timeout_duration: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
    if *cancel.borrow() {
        return Err(TorrentError::Cancelled);
    }
    tokio::select! {
        result = timeout(timeout_duration, stream.write_all(data)) => {
            result.map_err(|_| TorrentError::Timeout("writing to a peer"))??;
            Ok(())
        }
        changed = cancel.changed() => {
            if changed.is_ok() && *cancel.borrow() {
                Err(TorrentError::Cancelled)
            } else {
                Err(TorrentError::peer("cancellation channel closed"))
            }
        }
    }
}

async fn read_exact_cancel(
    stream: &mut TcpStream,
    data: &mut [u8],
    timeout_duration: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
    if *cancel.borrow() {
        return Err(TorrentError::Cancelled);
    }
    tokio::select! {
        result = timeout(timeout_duration, stream.read_exact(data)) => {
            result.map_err(|_| TorrentError::Timeout("reading from a peer"))??;
            Ok(())
        }
        changed = cancel.changed() => {
            if changed.is_ok() && *cancel.borrow() {
                Err(TorrentError::Cancelled)
            } else {
                Err(TorrentError::peer("cancellation channel closed"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_never_assigns_a_piece_twice() {
        let scheduler = PieceScheduler::new(vec![true, true, false]);
        let available = vec![true, true, true];
        let first = scheduler.claim(&available).unwrap();
        let second = scheduler.claim(&available).unwrap();
        assert_ne!(first, second);
        assert!(scheduler.claim(&available).is_none());
        scheduler.complete(first);
        scheduler.release(second);
        assert_eq!(scheduler.claim(&available), Some(second));
    }

    #[test]
    fn decodes_bitfields_most_significant_bit_first() {
        let mut available = vec![false; 10];
        update_bitfield(&[5, 0b1010_0000, 0b0100_0000], &mut available);
        assert!(available[0]);
        assert!(!available[1]);
        assert!(available[2]);
        assert!(available[9]);
    }
}
