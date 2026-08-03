use std::fmt::{Display, Formatter};
use std::io;

#[derive(Debug)]
pub enum TorrentError {
    Cancelled,
    Io(io::Error),
    Http(reqwest::Error),
    InvalidBencode(String),
    InvalidMetainfo(String),
    InvalidMagnet(String),
    InvalidProxy(String),
    Tracker(String),
    Peer(String),
    Timeout(&'static str),
    NoPeers,
    Unsupported(String),
}

impl TorrentError {
    pub(crate) fn bencode(message: impl Into<String>) -> Self {
        Self::InvalidBencode(message.into())
    }

    pub(crate) fn metainfo(message: impl Into<String>) -> Self {
        Self::InvalidMetainfo(message.into())
    }

    pub(crate) fn tracker(message: impl Into<String>) -> Self {
        Self::Tracker(message.into())
    }

    pub(crate) fn peer(message: impl Into<String>) -> Self {
        Self::Peer(message.into())
    }
}

impl Display for TorrentError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(f, "torrent operation cancelled"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Http(error) => write!(f, "HTTP error: {error}"),
            Self::InvalidBencode(message) => write!(f, "invalid bencode: {message}"),
            Self::InvalidMetainfo(message) => write!(f, "invalid torrent metadata: {message}"),
            Self::InvalidMagnet(message) => write!(f, "invalid magnet link: {message}"),
            Self::InvalidProxy(message) => write!(f, "invalid torrent proxy: {message}"),
            Self::Tracker(message) => write!(f, "tracker error: {message}"),
            Self::Peer(message) => write!(f, "peer error: {message}"),
            Self::Timeout(operation) => write!(f, "timed out while {operation}"),
            Self::NoPeers => write!(f, "no usable torrent peers were found"),
            Self::Unsupported(message) => write!(f, "unsupported torrent feature: {message}"),
        }
    }
}

impl std::error::Error for TorrentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Http(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for TorrentError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<reqwest::Error> for TorrentError {
    fn from(value: reqwest::Error) -> Self {
        Self::Http(value)
    }
}

pub type Result<T> = std::result::Result<T, TorrentError>;
