#[derive(Clone)]
pub enum TorrentType {
    Magnet(String),
    Link(String),
    GDrive(String),
    Direct(String),
}

impl TorrentType {
    pub fn get(&self) -> String {
        match self {
            TorrentType::Link(value)
            | TorrentType::Magnet(value)
            | TorrentType::GDrive(value)
            | TorrentType::Direct(value) => value.clone(),
        }
    }

    pub fn get_arg(&self) -> String {
        match self {
            TorrentType::Magnet(_) => "magnet".to_string(),
            TorrentType::GDrive(_) => "gdrive".to_string(),
            TorrentType::Direct(_) => "direct".to_string(),
            TorrentType::Link(_) => "nomagnet".to_string(),
        }
    }

    pub fn display(&self) -> String {
        match self {
            TorrentType::Link(value)
            | TorrentType::GDrive(value)
            | TorrentType::Direct(value) => display_source_link(value),
            TorrentType::Magnet(_) => "Magnet link hidden".to_string(),
        }
    }
}

// Nyaa links are downloaded through the canonical .torrent endpoint, while user-facing messages
// and SOURCE.md use the corresponding view page on the same Nyaa host.
pub fn display_source_link(input: &str) -> String {
    nyaa_link_parts(input)
        .map(|(origin, id)| format!("{}/view/{}", origin, id))
        .unwrap_or_else(|| input.trim().to_string())
}

fn nyaa_download_link(input: &str) -> Option<String> {
    nyaa_link_parts(input).map(|(origin, id)| format!("{}/download/{}.torrent", origin, id))
}

fn nyaa_link_parts(input: &str) -> Option<(String, String)> {
    let url = reqwest::Url::parse(input.trim()).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?;
    if host != "nyaa.si" && host != "nyaa.land" {
        return None;
    }
    let segments = url
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let raw_id = match segments.as_slice() {
        ["download", id] => *id,
        ["view", id] => *id,
        ["view", id, "torrent"] => *id,
        _ => return None,
    };
    let id = raw_id.strip_suffix(".torrent").unwrap_or(raw_id);
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((format!("{}://{}", url.scheme(), host), id.to_string()))
}

fn is_direct_video_url(input: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(input) else {
        return false;
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    let Some(segment) = url.path_segments().and_then(|mut segments| segments.next_back()) else {
        return false;
    };
    let lower = segment.to_ascii_lowercase();
    ["mkv", "mp4", "m4v", "mov", "avi", "webm", "ts", "m2ts"]
        .iter()
        .any(|extension| lower.ends_with(&format!(".{}", extension)))
}

pub fn nyaaise(input: &str) -> TorrentType {
    let input = input.trim();
    if let Some(download) = nyaa_download_link(input) {
        println!("{}", download);
        TorrentType::Link(download)
    } else if input.starts_with("magnet:") {
        TorrentType::Magnet(input.to_string())
    } else if input.starts_with("https://drive.google.com/")
        || input.starts_with("https://drive.usercontent.google.com/")
    {
        TorrentType::GDrive(input.to_string())
    } else if is_direct_video_url(input) {
        TorrentType::Direct(input.to_string())
    } else {
        TorrentType::Link(input.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{display_source_link, nyaaise, TorrentType};

    fn link_value(value: TorrentType) -> String {
        match value {
            TorrentType::Link(link) => link,
            _ => panic!("link not detected"),
        }
    }

    #[test]
    fn nyaa_download_torrent_is_canonicalized_for_fetching() {
        assert_eq!(
            link_value(nyaaise("https://nyaa.si/download/2075946.torrent")),
            "https://nyaa.si/download/2075946.torrent"
        );
    }

    #[test]
    fn nyaa_view_variants_are_canonicalized_for_fetching() {
        for link in [
            "https://nyaa.si/view/2075946/torrent",
            "https://nyaa.si/view/2075946",
            "https://nyaa.si/download/2075946",
            "https://nyaa.si/view/2075946/",
        ] {
            assert_eq!(
                link_value(nyaaise(link)),
                "https://nyaa.si/download/2075946.torrent"
            );
        }
    }

    #[test]
    fn nyaa_land_keeps_its_host() {
        assert_eq!(
            link_value(nyaaise("https://nyaa.land/view/1234")),
            "https://nyaa.land/download/1234.torrent"
        );
        assert_eq!(
            display_source_link("https://nyaa.land/download/1234.torrent"),
            "https://nyaa.land/view/1234"
        );
    }

    #[test]
    fn nyaa_displays_as_view_page() {
        assert_eq!(
            display_source_link("https://nyaa.si/download/2075946.torrent"),
            "https://nyaa.si/view/2075946"
        );
        let torrent = nyaaise("https://nyaa.si/view/2075946");
        assert_eq!(torrent.display(), "https://nyaa.si/view/2075946");
    }

    #[test]
    fn malformed_nyaa_link_is_not_replaced_with_a_blank_source() {
        assert_eq!(
            link_value(nyaaise("https://nyaa.si/view/not-a-number")),
            "https://nyaa.si/view/not-a-number"
        );
    }

    #[test]
    fn magnet_is_detected() {
        let link = "magnet:?xt=urn:btih:109c9fc9ffbc4c320296d0569db67c451f49c069";
        match nyaaise(link) {
            TorrentType::Magnet(value) => assert_eq!(value, link),
            _ => panic!("magnet not detected"),
        }
    }

    #[test]
    fn direct_video_is_detected() {
        let link = "https://example.com/video/input.mkv";
        match nyaaise(link) {
            TorrentType::Direct(value) => assert_eq!(value, link),
            _ => panic!("direct video not detected"),
        }
    }
}
