
// magnet:?xt=urn:btih:109c9fc9ffbc4c320296d0569db67c451f49c069&dn=%5BErai-raws%5D%20Hell%20Mode%3A%20Yarikomizuki%20no%20Gamer%20wa%20Hai%20Settei%20no%20Isekai%20de%20Musou%20suru%20-%2007%20%5B720p%20ADN%20WEB-DL%20AVC%20AAC%5D%5BMultiSub%5D%5BA31978B8%5D&tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce
// https://nyaa.si/download/2075946.torrent
// https://nyaa.si/view/2075111/torrent
// https://nyaa.si/view/2070406
// https://nyaa.si/download/2070406

#[derive(Clone)]
pub enum TorrentType {
    Magnet(String),
    Link(String),
}
impl TorrentType {
    pub fn get(&self) -> String {
        match self {
            TorrentType::Link(a) => a.clone(),
            TorrentType::Magnet(a) => a.clone(),
        }
    }
    pub fn get_arg(&self) -> String {
        match self {
            TorrentType::Magnet(_) => "magnet".to_string(),
            _ => "nomagnet".to_string()
        }
    }
    pub fn display(&self) -> String {
        match self {
            TorrentType::Link(a) => a.clone(),
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

const PATTERNS: [Pattern; 4] = [
    Pattern { startswith: Some("https://nyaa.si/download/"), endswith: Some(".torrent"), method: Method::DoNothing },
    Pattern { startswith: Some("https://nyaa.si/view/"), endswith: Some("/torrent"), method: Method::Eliminate },
    Pattern { startswith: Some("https://nyaa.si/download/"), endswith: None, method: Method::Eliminate },
    Pattern { startswith: Some("https://nyaa.si/view/"), endswith: None, method: Method::Eliminate },
];

pub fn nyaaise(str: &str) -> (TorrentType, Option<String>) {

    if str.starts_with("https://nyaa") || str.starts_with("http://nyaa") {

        for patt in PATTERNS {
            match patt.is_match(str) {
                Ok(a) => {println!("{}", a);
                    return (TorrentType::Link(a), None)}
                Err(_) => {}
            }
        }
        return (TorrentType::Link(String::new()), Some(String::new()))
    } else if str.starts_with("magnet:") {
        return (TorrentType::Magnet(str.to_string()), Some(String::new()));
    }
    (TorrentType::Link(String::new()), Some(String::new()))
}

#[cfg(test)]
mod tests {
    use crate::libpnp2p::nyaaise::nyaaise;
    use crate::libpnp2p::nyaaise::TorrentType;

    #[test]
    fn nyaa_true() {
        let link = "https://nyaa.si/download/2075946.torrent";
        let expected = "https://nyaa.si/download/2075946.torrent";
        let ab = nyaaise(link);
        match ab.0 {
            TorrentType::Link(a) => {
                assert_eq!(expected, a);
            }
            TorrentType::Magnet(_) => {
                panic!("Magnet not expected")
            }
        }
    }
    #[test]
    fn nyaa_view_torrent() {
        let link = "https://nyaa.si/view/2075946/torrent";
        let expected = "https://nyaa.si/download/2075946.torrent";
        let ab = nyaaise(link);
        match ab.0 {
            TorrentType::Link(a) => {
                assert_eq!(expected, a);
            }
            TorrentType::Magnet(_) => {
                panic!("Magnet not expected")
            }
        }
    }
    #[test]
    fn nyaa_download() {
        let link = "https://nyaa.si/view/2075946";
        let expected = "https://nyaa.si/download/2075946.torrent";
        let ab = nyaaise(link);
        match ab.0 {
            TorrentType::Link(a) => {
                assert_eq!(expected, a);
            }
            TorrentType::Magnet(_) => {
                panic!("Magnet not expected")
            }
        }
    }
    #[test]
    fn nyaa_view() {
        let link = "https://nyaa.si/download/2075946";
        let expected = "https://nyaa.si/download/2075946.torrent";
        let ab = nyaaise(link);
        match ab.0 {
            TorrentType::Link(a) => {
                assert_eq!(expected, a);
            }
            TorrentType::Magnet(_) => {
                panic!("Magnet not expected")
            }
        }
    }
    #[test]
    fn magnet() {
        let link = "magnet:?xt=urn:btih:109c9fc9ffbc4c320296d0569db67c451f49c069&dn=%5BErai-raws%5D%20Hell%20Mode%3A%20Yarikomizuki%20no%20Gamer%20wa%20Hai%20Settei%20no%20Isekai%20de%20Musou%20suru%20-%2007%20%5B720p%20ADN%20WEB-DL%20AVC%20AAC%5D%5BMultiSub%5D%5BA31978B8%5D&tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce";
        let expected = "magnet:?xt=urn:btih:109c9fc9ffbc4c320296d0569db67c451f49c069&dn=%5BErai-raws%5D%20Hell%20Mode%3A%20Yarikomizuki%20no%20Gamer%20wa%20Hai%20Settei%20no%20Isekai%20de%20Musou%20suru%20-%2007%20%5B720p%20ADN%20WEB-DL%20AVC%20AAC%5D%5BMultiSub%5D%5BA31978B8%5D&tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce";
        let ab = nyaaise(link);
        match ab.0 {
            TorrentType::Link(_) => {
                panic!("Magnet not expected");
            }
            TorrentType::Magnet(a) => {
                assert_eq!(expected, a);
            }
        }
    }

    // magnet:?xt=urn:btih:109c9fc9ffbc4c320296d0569db67c451f49c069&dn=%5BErai-raws%5D%20Hell%20Mode%3A%20Yarikomizuki%20no%20Gamer%20wa%20Hai%20Settei%20no%20Isekai%20de%20Musou%20suru%20-%2007%20%5B720p%20ADN%20WEB-DL%20AVC%20AAC%5D%5BMultiSub%5D%5BA31978B8%5D&tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce
}
