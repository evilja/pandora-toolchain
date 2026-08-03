use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use reqwest::Url;
use sha1::{Digest, Sha1};

use super::bencode::{Value, decode, dictionary_value_range};
use super::error::{Result, TorrentError};

const MAX_PIECE_LENGTH: u64 = 64 * 1024 * 1024;
const MAX_FILES: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentFile {
    pub index: u64,
    pub path: PathBuf,
    pub length: u64,
    pub offset: u64,
}

#[derive(Clone, Debug)]
pub struct Metainfo {
    pub info_hash: [u8; 20],
    pub name: String,
    pub piece_length: u64,
    pub piece_hashes: Vec<[u8; 20]>,
    pub files: Vec<TorrentFile>,
    pub trackers: Vec<String>,
    pub total_length: u64,
    pub private: bool,
    pub(crate) info_bytes: Vec<u8>,
}

impl Metainfo {
    pub fn from_torrent_bytes(data: &[u8]) -> Result<Self> {
        let root = decode(data)?;
        let info_range = dictionary_value_range(data, b"info")?;
        let trackers = parse_trackers(&root);
        Self::from_info_bytes(&data[info_range], trackers, None)
    }

    pub fn from_info_bytes(
        info_bytes: &[u8],
        trackers: Vec<String>,
        expected_hash: Option<[u8; 20]>,
    ) -> Result<Self> {
        let info_hash = sha1_bytes(info_bytes);
        if expected_hash.is_some_and(|expected| expected != info_hash) {
            return Err(TorrentError::metainfo(
                "the metadata does not match the requested info hash",
            ));
        }
        let info = decode(info_bytes)?;
        let dictionary = info
            .as_dictionary()
            .ok_or_else(|| TorrentError::metainfo("the info value is not a dictionary"))?;

        let name_bytes = preferred_bytes(&info, b"name.utf-8", b"name")
            .ok_or_else(|| TorrentError::metainfo("info.name is missing"))?;
        let name = safe_component(name_bytes)?;
        let piece_length = required_positive_integer(&info, b"piece length")?;
        let piece_length = u64::try_from(piece_length)
            .map_err(|_| TorrentError::metainfo("piece length is negative"))?;
        if piece_length == 0 || piece_length > MAX_PIECE_LENGTH {
            return Err(TorrentError::metainfo(format!(
                "piece length must be between 1 and {MAX_PIECE_LENGTH} bytes"
            )));
        }
        let pieces = dictionary
            .get(b"pieces".as_slice())
            .and_then(Value::as_bytes)
            .ok_or_else(|| TorrentError::metainfo("info.pieces is missing"))?;
        if pieces.len() % 20 != 0 {
            return Err(TorrentError::metainfo(
                "info.pieces is not a sequence of SHA-1 hashes",
            ));
        }
        let piece_hashes = pieces
            .chunks_exact(20)
            .map(|piece| {
                let mut hash = [0u8; 20];
                hash.copy_from_slice(piece);
                hash
            })
            .collect::<Vec<_>>();

        let mut files = Vec::new();
        let mut offset = 0u64;
        if let Some(length) = dictionary.get(b"length".as_slice()) {
            let length = non_negative_integer(length, "info.length")?;
            let path = PathBuf::from(&name);
            validate_relative_path(&path)?;
            files.push(TorrentFile {
                index: 0,
                path,
                length,
                offset,
            });
            offset = length;
        } else {
            let list = dictionary
                .get(b"files".as_slice())
                .and_then(Value::as_list)
                .ok_or_else(|| TorrentError::metainfo("info.files is missing"))?;
            if list.is_empty() || list.len() > MAX_FILES {
                return Err(TorrentError::metainfo(
                    "the multi-file torrent has an invalid file count",
                ));
            }
            let mut seen_paths = HashSet::new();
            for (index, value) in list.iter().enumerate() {
                let file = value.as_dictionary().ok_or_else(|| {
                    TorrentError::metainfo("an info.files entry is not a dictionary")
                })?;
                let length = file
                    .get(b"length".as_slice())
                    .ok_or_else(|| TorrentError::metainfo("a file length is missing"))?;
                let length = non_negative_integer(length, "file length")?;
                let path_value = file
                    .get(b"path.utf-8".as_slice())
                    .or_else(|| file.get(b"path".as_slice()))
                    .and_then(Value::as_list)
                    .ok_or_else(|| TorrentError::metainfo("a file path is missing"))?;
                if path_value.is_empty() {
                    return Err(TorrentError::metainfo("a file path is empty"));
                }
                let mut path = PathBuf::from(&name);
                for component in path_value {
                    let bytes = component.as_bytes().ok_or_else(|| {
                        TorrentError::metainfo("a file path component is not a byte string")
                    })?;
                    path.push(safe_component(bytes)?);
                }
                validate_relative_path(&path)?;
                if !seen_paths.insert(path.clone()) {
                    return Err(TorrentError::metainfo(format!(
                        "duplicate file path {}",
                        path.display()
                    )));
                }
                files.push(TorrentFile {
                    index: u64::try_from(index)
                        .map_err(|_| TorrentError::metainfo("file index overflow"))?,
                    path,
                    length,
                    offset,
                });
                offset = offset
                    .checked_add(length)
                    .ok_or_else(|| TorrentError::metainfo("total torrent size overflow"))?;
            }
        }

        let expected_piece_count = if offset == 0 {
            0
        } else {
            offset
                .checked_add(piece_length - 1)
                .ok_or_else(|| TorrentError::metainfo("piece count overflow"))?
                / piece_length
        };
        if usize::try_from(expected_piece_count).ok() != Some(piece_hashes.len()) {
            return Err(TorrentError::metainfo(format!(
                "piece hash count {} does not match total size {}",
                piece_hashes.len(),
                offset
            )));
        }

        let private = dictionary
            .get(b"private".as_slice())
            .and_then(Value::as_integer)
            == Some(1);
        let mut trackers = trackers;
        trackers.retain(|tracker| {
            Url::parse(tracker)
                .ok()
                .is_some_and(|url| matches!(url.scheme(), "http" | "https" | "udp"))
        });
        deduplicate(&mut trackers);

        Ok(Self {
            info_hash,
            name,
            piece_length,
            piece_hashes,
            files,
            trackers,
            total_length: offset,
            private,
            info_bytes: info_bytes.to_vec(),
        })
    }

    pub fn piece_size(&self, index: usize) -> Option<usize> {
        if index >= self.piece_hashes.len() {
            return None;
        }
        let start = u64::try_from(index).ok()?.checked_mul(self.piece_length)?;
        let remaining = self.total_length.checked_sub(start)?;
        usize::try_from(remaining.min(self.piece_length)).ok()
    }

    pub fn selected_pieces(&self, selected_files: Option<&HashSet<u64>>) -> Result<Vec<bool>> {
        if let Some(selected) = selected_files {
            for index in selected {
                if !self.files.iter().any(|file| file.index == *index) {
                    return Err(TorrentError::metainfo(format!(
                        "selected file index {index} does not exist"
                    )));
                }
            }
        }
        let mut pieces = vec![false; self.piece_hashes.len()];
        for file in &self.files {
            if selected_files.is_some_and(|selected| !selected.contains(&file.index))
                || file.length == 0
            {
                continue;
            }
            let first = file.offset / self.piece_length;
            let last = (file.offset + file.length - 1) / self.piece_length;
            for piece in first..=last {
                let piece = usize::try_from(piece)
                    .map_err(|_| TorrentError::metainfo("piece index overflow"))?;
                pieces[piece] = true;
            }
        }
        Ok(pieces)
    }

    pub fn selected_length(&self, selected_files: Option<&HashSet<u64>>) -> u64 {
        self.files
            .iter()
            .filter(|file| selected_files.is_none_or(|selected| selected.contains(&file.index)))
            .map(|file| file.length)
            .sum()
    }

    pub fn info_bytes(&self) -> &[u8] {
        &self.info_bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Magnet {
    pub info_hash: [u8; 20],
    pub trackers: Vec<String>,
    pub display_name: Option<String>,
}

impl Magnet {
    pub fn parse(value: &str) -> Result<Self> {
        let url =
            Url::parse(value).map_err(|error| TorrentError::InvalidMagnet(error.to_string()))?;
        if url.scheme() != "magnet" {
            return Err(TorrentError::InvalidMagnet(
                "URL scheme is not magnet".to_string(),
            ));
        }
        let mut info_hash = None;
        let mut trackers = Vec::new();
        let mut display_name = None;
        for (key, value) in url.query_pairs() {
            if key.eq_ignore_ascii_case("xt") {
                if value
                    .get(..9)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("urn:btih:"))
                {
                    info_hash = Some(parse_btih(&value[9..])?);
                }
            } else if key.eq_ignore_ascii_case("tr") {
                trackers.push(value.into_owned());
            } else if key.eq_ignore_ascii_case("dn") {
                display_name = Some(value.into_owned());
            }
        }
        deduplicate(&mut trackers);
        Ok(Self {
            info_hash: info_hash.ok_or_else(|| {
                TorrentError::InvalidMagnet("v1 xt=urn:btih parameter is missing".to_string())
            })?,
            trackers,
            display_name,
        })
    }
}

pub fn magnet_info_hash(value: &str) -> Option<String> {
    Magnet::parse(value)
        .ok()
        .map(|magnet| hex_hash(&magnet.info_hash))
}

pub fn torrent_info_hash(data: &[u8]) -> Option<String> {
    let range = dictionary_value_range(data, b"info").ok()?;
    Some(hex_hash(&sha1_bytes(&data[range])))
}

pub fn hex_hash(hash: &[u8; 20]) -> String {
    let mut output = String::with_capacity(40);
    for byte in hash {
        use std::fmt::Write;
        write!(&mut output, "{byte:02x}").ok();
    }
    output
}

fn parse_trackers(root: &Value) -> Vec<String> {
    let mut trackers = Vec::new();
    if let Some(announce) = root.get(b"announce").and_then(Value::as_bytes) {
        trackers.push(String::from_utf8_lossy(announce).into_owned());
    }
    if let Some(tiers) = root.get(b"announce-list").and_then(Value::as_list) {
        for tier in tiers {
            if let Some(values) = tier.as_list() {
                for tracker in values {
                    if let Some(tracker) = tracker.as_bytes() {
                        trackers.push(String::from_utf8_lossy(tracker).into_owned());
                    }
                }
            } else if let Some(tracker) = tier.as_bytes() {
                trackers.push(String::from_utf8_lossy(tracker).into_owned());
            }
        }
    }
    deduplicate(&mut trackers);
    trackers
}

fn preferred_bytes<'a>(value: &'a Value, preferred: &[u8], fallback: &[u8]) -> Option<&'a [u8]> {
    value
        .get(preferred)
        .or_else(|| value.get(fallback))
        .and_then(Value::as_bytes)
}

fn required_positive_integer(value: &Value, key: &[u8]) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_integer)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            TorrentError::metainfo(format!(
                "{} is missing or not positive",
                String::from_utf8_lossy(key)
            ))
        })
}

fn non_negative_integer(value: &Value, label: &str) -> Result<u64> {
    let value = value
        .as_integer()
        .ok_or_else(|| TorrentError::metainfo(format!("{label} is not an integer")))?;
    u64::try_from(value)
        .map_err(|_| TorrentError::metainfo(format!("{label} must not be negative")))
}

fn safe_component(bytes: &[u8]) -> Result<String> {
    let value = String::from_utf8_lossy(bytes).into_owned();
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(TorrentError::metainfo(format!(
            "unsafe path component {:?}",
            value
        )));
    }
    Ok(value)
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(TorrentError::metainfo(format!(
            "unsafe torrent path {}",
            path.display()
        )));
    }
    Ok(())
}

fn sha1_bytes(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn parse_btih(value: &str) -> Result<[u8; 20]> {
    if value.len() == 40 {
        let mut output = [0u8; 20];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            output[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
        }
        return Ok(output);
    }
    if value.len() == 32 {
        return decode_base32(value);
    }
    Err(TorrentError::InvalidMagnet(
        "BTIH must contain 40 hexadecimal or 32 base32 characters".to_string(),
    ))
}

fn hex_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(TorrentError::InvalidMagnet(
            "BTIH contains a non-hexadecimal character".to_string(),
        )),
    }
}

fn decode_base32(value: &str) -> Result<[u8; 20]> {
    let mut output = [0u8; 20];
    let mut accumulator = 0u32;
    let mut bits = 0u8;
    let mut position = 0usize;
    for byte in value.bytes() {
        let digit = match byte.to_ascii_uppercase() {
            b'A'..=b'Z' => byte.to_ascii_uppercase() - b'A',
            b'2'..=b'7' => byte - b'2' + 26,
            _ => {
                return Err(TorrentError::InvalidMagnet(
                    "BTIH contains an invalid base32 character".to_string(),
                ));
            }
        };
        accumulator = (accumulator << 5) | u32::from(digit);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            output[position] = (accumulator >> bits) as u8;
            position += 1;
            accumulator &= (1u32 << bits).wrapping_sub(1);
        }
    }
    if position != output.len() || bits != 0 {
        return Err(TorrentError::InvalidMagnet(
            "BTIH base32 length is invalid".to_string(),
        ));
    }
    Ok(output)
}

fn deduplicate(values: &mut Vec<String>) {
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single_file_torrent() -> Vec<u8> {
        b"d8:announce31:http://tracker.invalid/announce4:infod6:lengthi5e4:name8:test.mkv12:piece lengthi5e6:pieces20:01234567890123456789ee".to_vec()
    }

    #[test]
    fn parses_single_file_metainfo_and_hash() {
        let bytes = single_file_torrent();
        let meta = Metainfo::from_torrent_bytes(&bytes).unwrap();
        assert_eq!(meta.files.len(), 1);
        assert_eq!(meta.files[0].path, PathBuf::from("test.mkv"));
        assert_eq!(meta.total_length, 5);
        assert_eq!(meta.piece_size(0), Some(5));
        assert_eq!(torrent_info_hash(&bytes).unwrap().len(), 40);
    }

    #[test]
    fn parses_hex_and_base32_magnets() {
        let hex = "0123456789abcdef0123456789abcdef01234567";
        let magnet = Magnet::parse(&format!(
            "magnet:?xt=urn:btih:{hex}&tr=http%3A%2F%2Ft.invalid%2Fa"
        ))
        .unwrap();
        assert_eq!(hex_hash(&magnet.info_hash), hex);
        let base32 = Magnet::parse("magnet:?xt=urn:btih:AERUKZ4JVPG66AJDIVTYTK6N54ASGRLH").unwrap();
        assert_eq!(base32.info_hash, magnet.info_hash);
    }

    #[test]
    fn selects_boundary_pieces() {
        let info = b"d5:filesld6:lengthi3e4:pathl5:a.mkveed6:lengthi3e4:pathl5:b.mkveee4:name4:root12:piece lengthi4e6:pieces40:0000000000000000000011111111111111111111e";
        let meta = Metainfo::from_info_bytes(info, vec![], None).unwrap();
        assert_eq!(
            meta.selected_pieces(Some(&HashSet::from([1]))).unwrap(),
            vec![true, true]
        );
    }

    #[test]
    fn rejects_escaping_paths() {
        let info = b"d5:filesld6:lengthi1e4:pathl2:..4:evilee e4:name4:root12:piece lengthi1e6:pieces20:00000000000000000000e";
        assert!(Metainfo::from_info_bytes(info, vec![], None).is_err());
    }
}
