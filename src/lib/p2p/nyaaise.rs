
// magnet:?xt=urn:btih:109c9fc9ffbc4c320296d0569db67c451f49c069&dn=%5BErai-raws%5D%20Hell%20Mode%3A%20Yarikomizuki%20no%20Gamer%20wa%20Hai%20Settei%20no%20Isekai%20de%20Musou%20suru%20-%2007%20%5B720p%20ADN%20WEB-DL%20AVC%20AAC%5D%5BMultiSub%5D%5BA31978B8%5D&tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce
// https://nyaa.si/download/2075946.torrent
// https://nyaa.si/view/2075111/torrent
// https://nyaa.si/view/2070406
// https://nyaa.si/download/2070406

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
            TorrentType::Link(a) => a.clone(),
            TorrentType::Magnet(a) => a.clone(),
            TorrentType::GDrive(a) => a.clone(),
            TorrentType::Direct(a) => a.clone(),
        }
    }
    pub fn get_arg(&self) -> String {
        match self {
            TorrentType::Magnet(_) => "magnet".to_string(),
            TorrentType::GDrive(_) => "gdrive".to_string(),
            TorrentType::Direct(_) => "direct".to_string(),
            _ => "nomagnet".to_string()
        }
    }
    pub fn display(&self) -> String {
        match self {
            TorrentType::Link(a) => a.clone(),
            TorrentType::GDrive(a) => a.clone(),
            TorrentType::Direct(a) => a.clone(),
            TorrentType::Magnet(_) => String::from("Magnet linki gösterilmiyor.")
        }
    }
}
#[derive(Clone)]
enum Method {
    Eliminate,
    DoNothing,
}
#[derive(Clone)]
pub struct Pattern {
    startswith: Option<&'static str>,
    endswith: Option<&'static str>,
    method: Method,
}
impl Pattern {
    pub fn is_match(&self, string: &str) -> Result<String, ()> {
        let mut sw:Option<usize> = None;
        let mut ew:Option<usize> = None;
        if let Some(a) = self.startswith {
            if !string.starts_with(a) { return Err(()); }
            sw = Some(a.len());
        }
        if let Some(a) = self.endswith {
            if !string.ends_with(a) { return Err(()); }
            ew = Some(a.len());
        }
        match self.method {
            Method::DoNothing => {
                return Ok(string.to_string());
            }
            Method::Eliminate => {
                let mut mystring = string.to_string();
                if let Some(a) = sw {
                    mystring = mystring[a..].into();
                }
                if let Some(b) = ew {
                    mystring = mystring[..=mystring.len()-(b+1)].into()
                }
                return Ok(format!("https://nyaa.si/download/{}.torrent", mystring));
            }
        }
    }
}

const PATTERNS: [Pattern; 8] = [
    Pattern { startswith: Some("https://nyaa.si/download/"), endswith: Some(".torrent"), method: Method::DoNothing },
    Pattern { startswith: Some("https://nyaa.si/view/"), endswith: Some("/torrent"), method: Method::Eliminate },
    Pattern { startswith: Some("https://nyaa.si/download/"), endswith: None, method: Method::Eliminate },
    Pattern { startswith: Some("https://nyaa.si/view/"), endswith: None, method: Method::Eliminate },
    Pattern { startswith: Some("https://nyaa.land/download/"), endswith: Some(".torrent"), method: Method::DoNothing },
    Pattern { startswith: Some("https://nyaa.land/view/"), endswith: Some("/torrent"), method: Method::Eliminate },
    Pattern { startswith: Some("https://nyaa.land/download/"), endswith: None, method: Method::Eliminate },
    Pattern { startswith: Some("https://nyaa.land/view/"), endswith: None, method: Method::Eliminate },
];

fn is_direct_video_url(input: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(input) else {
        return false;
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    let Some(segment) = url.path_segments().and_then(|mut s| s.next_back()) else {
        return false;
    };
    let lower = segment.to_ascii_lowercase();
    ["mkv", "mp4", "m4v", "mov", "avi", "webm", "ts", "m2ts"]
        .iter()
        .any(|ext| lower.ends_with(&format!(".{}", ext)))
}

pub fn nyaaise(str: &str) -> TorrentType {

    if str.starts_with("https://nyaa") {

        for patt in PATTERNS {
            match patt.is_match(str) {
                Ok(a) => {println!("{}", a);
                    return TorrentType::Link(a)}
                Err(_) => {}
            }
        }
        return TorrentType::Link(String::new())
    } else if str.starts_with("magnet:") {
        return TorrentType::Magnet(str.to_string())
    } else if str.starts_with("https://drive.google.com/")
        || str.starts_with("https://drive.usercontent.google.com/")
    {
        return TorrentType::GDrive(str.to_string())
    } else if is_direct_video_url(str) {
        return TorrentType::Direct(str.to_string())
    }
    TorrentType::Link(str.to_string())
}

#[cfg(test)]
mod tests {
    use crate::lib::p2p::nyaaise::nyaaise;
    use crate::lib::p2p::nyaaise::TorrentType;

    #[test]
    fn nyaa_true() {
        let link = "https://nyaa.si/download/2075946.torrent";
        let expected = "https://nyaa.si/download/2075946.torrent";
        let ab = nyaaise(link);
        match ab {
            TorrentType::Link(a) => {
                assert_eq!(expected, a);
            }
            TorrentType::Magnet(_) => {
                panic!("Magnet not expected")
            }
            TorrentType::GDrive(_) => {
                panic!("GDrive not expected")
            }
            TorrentType::Direct(_) => {
                panic!("Direct not expected")
            }
        }
    }
    #[test]
    fn nyaa_view_torrent() {
        let link = "https://nyaa.si/view/2075946/torrent";
        let expected = "https://nyaa.si/download/2075946.torrent";
        let ab = nyaaise(link);
        match ab {
            TorrentType::Link(a) => {
                assert_eq!(expected, a);
            }
            TorrentType::Magnet(_) => {
                panic!("Magnet not expected")
            }
            TorrentType::GDrive(_) => {
                panic!("GDrive not expected")
            }
            TorrentType::Direct(_) => {
                panic!("Direct not expected")
            }
        }
    }
    #[test]
    fn nyaa_download() {
        let link = "https://nyaa.si/view/2075946";
        let expected = "https://nyaa.si/download/2075946.torrent";
        let ab = nyaaise(link);
        match ab {
            TorrentType::Link(a) => {
                assert_eq!(expected, a);
            }
            TorrentType::Magnet(_) => {
                panic!("Magnet not expected")
            }
            TorrentType::GDrive(_) => {
                panic!("GDrive not expected")
            }
            TorrentType::Direct(_) => {
                panic!("Direct not expected")
            }
        }
    }
    #[test]
    fn nyaa_view() {
        let link = "https://nyaa.si/download/2075946";
        let expected = "https://nyaa.si/download/2075946.torrent";
        let ab = nyaaise(link);
        match ab {
            TorrentType::Link(a) => {
                assert_eq!(expected, a);
            }
            TorrentType::Magnet(_) => {
                panic!("Magnet not expected")
            }
            TorrentType::GDrive(_) => {
                panic!("GDrive not expected")
            }
            TorrentType::Direct(_) => {
                panic!("Direct not expected")
            }
        }
    }
    #[test]
    fn magnet() {
        let link = "magnet:?xt=urn:btih:109c9fc9ffbc4c320296d0569db67c451f49c069&dn=%5BErai-raws%5D%20Hell%20Mode%3A%20Yarikomizuki%20no%20Gamer%20wa%20Hai%20Settei%20no%20Isekai%20de%20Musou%20suru%20-%2007%20%5B720p%20ADN%20WEB-DL%20AVC%20AAC%5D%5BMultiSub%5D%5BA31978B8%5D&tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce";
        let expected = "magnet:?xt=urn:btih:109c9fc9ffbc4c320296d0569db67c451f49c069&dn=%5BErai-raws%5D%20Hell%20Mode%3A%20Yarikomizuki%20no%20Gamer%20wa%20Hai%20Settei%20no%20Isekai%20de%20Musou%20suru%20-%2007%20%5B720p%20ADN%20WEB-DL%20AVC%20AAC%5D%5BMultiSub%5D%5BA31978B8%5D&tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce";
        let ab = nyaaise(link);
        match ab {
            TorrentType::Link(_) => {
                panic!("Magnet not expected");
            }
            TorrentType::Magnet(a) => {
                assert_eq!(expected, a);
            }
            TorrentType::GDrive(_) => {
                panic!("GDrive not expected")
            }
            TorrentType::Direct(_) => {
                panic!("Direct not expected")
            }
        }
    }

    #[test]
    fn direct_video() {
        let link = "https://pub9.animeout.com/series/00RAPIDBOT/Snack%20Basue/[AnimeOut]%20Snack%20Basue%20-%2008%201080pp%20[4EE8E501][1080pp][SubsPlease][RapidBot].mkv";
        let ab = nyaaise(link);
        match ab {
            TorrentType::Direct(a) => {
                assert_eq!(link, a);
            }
            _ => {
                panic!("Direct not detected")
            }
        }
    }

    // magnet:?xt=urn:btih:109c9fc9ffbc4c320296d0569db67c451f49c069&dn=%5BErai-raws%5D%20Hell%20Mode%3A%20Yarikomizuki%20no%20Gamer%20wa%20Hai%20Settei%20no%20Isekai%20de%20Musou%20suru%20-%2007%20%5B720p%20ADN%20WEB-DL%20AVC%20AAC%5D%5BMultiSub%5D%5BA31978B8%5D&tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce
}
