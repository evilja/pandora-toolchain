// A channel can watch a release feed — a Nyaa search or any RSS feed — and have each episode's
// `SOURCE.md` written as soon as a matching release appears. Everything here is the part that
// needs no Discord: where the watch lives on disk, reading the feed, and the one hard question a
// feed poses, which is what episode a release title is.
//
// That question is hard because fansubs and raw groups do not agree on numbering. A channel is
// attached to one MyAnimeList entry, and its repo counts that entry's episodes from 1. A group
// numbering a whole franchise straight through calls the first episode of the second season `64`,
// and nothing in `Show - 64` says so. The answer is kept per channel as an offset — release number
// minus offset is the repo's episode — which the bot guesses and the person confirms, so nobody is
// ever asked to type one.

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::path::PathBuf;
use std::time::Duration;

const WATCH_FILE: &str = "watch.toml";
// Feed items already handled, newest last. A Nyaa feed carries 75 items, so a few hundred keys
// cover every item a feed could still be showing.
const SEEN_LIMIT: usize = 300;
// Releases waiting on a person's answer. Past this the oldest question is dropped: a channel with
// twenty unanswered ones has a feed that matches the wrong show, and the status view says so.
const PENDING_LIMIT: usize = 20;
// A feed is a page of text. Anything this large is not one, and is refused before it is parsed.
const FEED_MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct WatchConfig {
    // The anime the watch was set up for. A channel re-attached to something else keeps its file
    // but the watch stops applying, since its numbering described another show.
    pub mal_id: u64,
    // What was typed, shown back in the status view.
    pub feed: String,
    // The RSS URL that is actually fetched.
    pub feed_url: String,
    // Release number minus this is the repo's episode. Zero when the numbers already agree.
    pub offset: u32,
    // The season the feed calls this anime, for titles that name one (`S2 - 05`, `S02E05`).
    pub season: u32,
    // A watch does nothing until its numbering has been confirmed.
    pub confirmed: bool,
    pub created_by: u64,
    // The setup message whose buttons confirm this watch. A second `/watch feed:` replaces the
    // watch, and the first message's buttons must not confirm the second one.
    #[serde(default)]
    pub setup_message: Option<u64>,
    #[serde(default)]
    pub last_checked: Option<i64>,
    #[serde(default)]
    pub last_release: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub seen: Vec<String>,
    // Kept last: TOML writes arrays of tables after every plain value.
    #[serde(default)]
    pub pending: Vec<PendingRelease>,
}

// A release the bot could not place on its own and has asked about in the channel.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PendingRelease {
    pub key: String,
    pub title: String,
    pub link: String,
    pub number: u32,
}

impl WatchConfig {
    pub fn has_seen(&self, key: &str) -> bool {
        self.seen.iter().any(|seen| seen == key)
    }

    pub fn mark_seen(&mut self, key: &str) {
        if self.has_seen(key) {
            return;
        }
        self.seen.push(key.to_string());
        if self.seen.len() > SEEN_LIMIT {
            let excess = self.seen.len() - SEEN_LIMIT;
            self.seen.drain(..excess);
        }
    }

    pub fn add_pending(&mut self, release: PendingRelease) {
        if self.pending.iter().any(|pending| pending.key == release.key) {
            return;
        }
        self.pending.push(release);
        if self.pending.len() > PENDING_LIMIT {
            let excess = self.pending.len() - PENDING_LIMIT;
            self.pending.drain(..excess);
        }
    }

    pub fn take_pending(&mut self, key: &str) -> Option<PendingRelease> {
        let position = self.pending.iter().position(|pending| pending.key == key)?;
        Some(self.pending.remove(position))
    }

    // The watch applies to this channel only while the channel is still attached to the anime it
    // was set up for.
    pub fn applies_to(&self, mal_id: Option<u64>) -> bool {
        mal_id == Some(self.mal_id)
    }
}

fn watch_path(server_id: u64, channel_id: u64) -> PathBuf {
    PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join(channel_id.to_string())
        .join(WATCH_FILE)
}

pub fn load(server_id: u64, channel_id: u64) -> Option<WatchConfig> {
    let text = std::fs::read_to_string(watch_path(server_id, channel_id)).ok()?;
    toml::from_str(&text).ok()
}

pub async fn save(server_id: u64, channel_id: u64, config: &WatchConfig) -> Result<(), String> {
    let path = watch_path(server_id, channel_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
    }
    let text = toml::to_string(config).map_err(|e| e.to_string())?;
    // Written beside and renamed over, so a poll reading it mid-write never sees half a file.
    let temp = path.with_extension("toml.tmp");
    tokio::fs::write(&temp, text).await.map_err(|e| e.to_string())?;
    tokio::fs::rename(&temp, &path).await.map_err(|e| e.to_string())
}

pub async fn remove(server_id: u64, channel_id: u64) {
    tokio::fs::remove_file(watch_path(server_id, channel_id)).await.ok();
}

// Every channel with a watch file, as `(server, channel)`.
pub fn all() -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let Ok(servers) = std::fs::read_dir(PathBuf::from("DB").join("config")) else {
        return out;
    };
    for server in servers.flatten() {
        let Some(server_id) = server.file_name().to_str().and_then(|s| s.parse::<u64>().ok()) else {
            continue;
        };
        let Ok(channels) = std::fs::read_dir(server.path()) else {
            continue;
        };
        for channel in channels.flatten() {
            let Some(channel_id) = channel.file_name().to_str().and_then(|s| s.parse::<u64>().ok()) else {
                continue;
            };
            if channel.path().join(WATCH_FILE).is_file() {
                out.push((server_id, channel_id));
            }
        }
    }
    out
}

// What `/watch feed:` accepts, turned into the RSS URL to fetch. A Nyaa page — a search, a user's
// uploads — is its own feed with `page=rss` added; any other URL is taken to be a feed already;
// and anything that is not a URL is a Nyaa search, across all anime, for that text.
pub fn feed_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("the feed is empty".to_string());
    }
    if !input.starts_with("http://") && !input.starts_with("https://") {
        let mut url = reqwest::Url::parse("https://nyaa.si/").unwrap();
        url.query_pairs_mut()
            .append_pair("page", "rss")
            .append_pair("q", input)
            .append_pair("c", "1_0")
            .append_pair("f", "0");
        return Ok(url.to_string());
    }
    let mut url = reqwest::Url::parse(input).map_err(|e| format!("not a valid link: {}", e))?;
    let is_nyaa = url
        .host_str()
        .is_some_and(|host| host == "nyaa.si" || host.ends_with(".nyaa.si"));
    if is_nyaa && !url.query_pairs().any(|(key, value)| key == "page" && value == "rss") {
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "page")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut().clear().append_pair("page", "rss").extend_pairs(kept);
    }
    Ok(url.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedItem {
    // Stable identity for "already handled": a short hash of the guid (or the link without one),
    // short enough to ride in a Discord component id.
    pub key: String,
    pub title: String,
    pub link: String,
}

// RSS 2.0 items, in feed order (newest first on every feed that matters here). Only `title`,
// `link` and `guid` are read; an item without a title or a link is not a release.
pub fn parse_feed(xml: &str) -> Vec<FeedItem> {
    let item_re = regex::Regex::new(r"(?s)<item\b[^>]*>(.*?)</item>").unwrap();
    item_re
        .captures_iter(xml)
        .filter_map(|caps| {
            let body = caps.get(1)?.as_str();
            let title = xml_field(body, "title")?;
            let guid = xml_field(body, "guid");
            // Nyaa's `<link>` is the `.torrent` and its `<guid>` the `/view/` page. Either is a
            // source `nyaaise` understands; the view page is what a person would have pasted.
            let link = xml_field(body, "link")
                .filter(|link| link.starts_with("http") || link.starts_with("magnet:"))
                .or_else(|| guid.clone().filter(|guid| guid.starts_with("http")))?;
            let identity = guid.unwrap_or_else(|| link.clone());
            Some(FeedItem {
                key: short_key(&identity),
                title,
                link,
            })
        })
        .collect()
}

fn xml_field(body: &str, tag: &str) -> Option<String> {
    let re = regex::Regex::new(&format!(r"(?s)<{tag}\b[^>]*>(.*?)</{tag}>", tag = tag)).unwrap();
    let raw = re.captures(body)?.get(1)?.as_str().trim();
    let raw = raw
        .strip_prefix("<![CDATA[")
        .and_then(|rest| rest.strip_suffix("]]>"))
        .unwrap_or(raw);
    let text = unescape_xml(raw.trim());
    (!text.is_empty()).then_some(text)
}

fn unescape_xml(text: &str) -> String {
    let numeric = regex::Regex::new(r"&#(x[0-9a-fA-F]+|[0-9]+);").unwrap();
    let decoded = numeric.replace_all(text, |caps: &regex::Captures| {
        let code = &caps[1];
        let value = match code.strip_prefix('x') {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => code.parse::<u32>().ok(),
        };
        value
            .and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_else(|| caps[0].to_string())
    });
    decoded
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn short_key(identity: &str) -> String {
    let digest = Sha1::digest(identity.trim().as_bytes());
    digest.iter().take(5).map(|byte| format!("{:02x}", byte)).collect()
}

// The feed is a URL a person typed, so it goes through the same guard as every other fetch of a
// user-supplied link: https only, no private addresses, and no redirects that could lead to one.
pub async fn fetch_feed(url: &str) -> Result<Vec<FeedItem>, String> {
    let url = crate::lib::http::net::sanitize_fetch_url(url).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Pandora/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("the feed answered {}", resp.status()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        body.extend_from_slice(&chunk);
        if body.len() > FEED_MAX_BYTES {
            return Err("the feed is too large to be an RSS feed".to_string());
        }
    }
    let text = String::from_utf8_lossy(&body);
    if !text.contains("<rss") && !text.contains("<channel") {
        return Err("that link is not an RSS feed".to_string());
    }
    Ok(parse_feed(&text))
}

// What a release title says about which episode it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseNumber {
    pub number: u32,
    // The season the title names, when it names one. A number with a season beside it is already
    // counted within that season and never goes through the offset.
    pub season: Option<u32>,
    // `v2` and up: a corrected re-release of an episode that may already have a source.
    pub version: u32,
}

// The episode a release title carries, or `None` for a title that is not one episode — a batch,
// a range, a movie, a recap numbered `12.5`. `None` means "not an episode", so it must only be
// returned for titles that really are not; a title that is an episode but cannot be placed is a
// question for a person, and that comes from `map_release`, not from here.
pub fn release_number(title: &str) -> Option<ReleaseNumber> {
    let lower = title.to_lowercase();
    if lower.contains("batch") || lower.contains("complete") {
        return None;
    }

    // `S02E05` and SubsPlease's `S2 - 05` carry their season.
    let seasonal = [
        r"(?i)\bS(\d{1,2})\s?E(\d{1,4})(?:v(\d{1,2}))?\b",
        r"(?i)\bS(\d{1,2})\s+-\s+(\d{1,4})(?:v(\d{1,2}))?\b",
    ];
    for pattern in seasonal {
        let re = regex::Regex::new(pattern).unwrap();
        if let Some(caps) = re.captures(title) {
            let whole = caps.get(0).unwrap();
            if is_range_or_fraction(&title[whole.end()..]) {
                return None;
            }
            return Some(ReleaseNumber {
                number: caps[2].parse().ok()?,
                season: caps[1].parse().ok(),
                version: caps.get(3).and_then(|v| v.as_str().parse().ok()).unwrap_or(1),
            });
        }
    }

    // `Show - 64`, taking the last dash so a title with a dash of its own (`86 - Eighty Six - 05`)
    // reads the episode rather than the name.
    let dashed = regex::Regex::new(r"\s-\s+(\d{1,4})(?:v(\d{1,2}))?\b").unwrap();
    let episode_word = regex::Regex::new(r"(?i)(?:^|[\s._\[(])(?:E|EP|Episode)\s?(\d{1,4})(?:v(\d{1,2}))?\b").unwrap();
    let caps = dashed
        .captures_iter(title)
        .last()
        .or_else(|| episode_word.captures(title))?;
    let whole = caps.get(0).unwrap();
    if is_range_or_fraction(&title[whole.end()..]) {
        return None;
    }
    Some(ReleaseNumber {
        number: caps[1].parse().ok()?,
        season: season_words(&title[..whole.start()]),
        version: caps.get(2).and_then(|v| v.as_str().parse().ok()).unwrap_or(1),
    })
}

// `01-12`, `01 ~ 12`, `12.5`: what follows the number says it is not one whole episode.
fn is_range_or_fraction(rest: &str) -> bool {
    regex::Regex::new(r"^(?:\s*[-~]\s*\d|\.\d)").unwrap().is_match(rest)
}

// `2nd Season`, `Season 2`, `S2` written before the number rather than beside it.
fn season_words(prefix: &str) -> Option<u32> {
    let patterns = [
        r"(?i)\b(\d{1,2})(?:st|nd|rd|th)\s+Season\b",
        r"(?i)\bSeason\s+(\d{1,2})\b",
        r"(?i)\bS(\d{1,2})\b",
    ];
    patterns.iter().find_map(|pattern| {
        regex::Regex::new(pattern)
            .unwrap()
            .captures_iter(prefix)
            .last()
            .and_then(|caps| caps[1].parse().ok())
    })
}

// The repo episode a release is, or `None` when it is not one of this season's episodes.
pub fn map_release(release: &ReleaseNumber, season: u32, episode_count: u32, offset: u32) -> Option<u32> {
    let episode = match release.season {
        Some(named) if named != season => return None,
        Some(_) => release.number,
        None => release.number.checked_sub(offset)?,
    };
    (1..=episode_count).contains(&episode).then_some(episode)
}

// The season the feed calls this anime: the one every season-naming title agrees on, and the
// channel's own when they do not agree or none names one. A channel attached without `season:`
// is season 1 even when it is really the sequel, and the feed knows better.
pub fn feed_season(releases: &[ReleaseNumber], channel_season: u32) -> u32 {
    let mut named = releases.iter().filter_map(|release| release.season);
    match named.next() {
        Some(first) if named.all(|season| season == first) => first,
        _ => channel_season,
    }
}

// The bot's guess at the offset, which a person confirms before anything is written.
// `bare` are the release numbers that carry no season; `prequel_total` is how many episodes
// MyAnimeList counts before this entry, when it could be asked.
pub fn guess_offset(bare: &[u32], episode_count: u32, prequel_total: Option<u32>) -> u32 {
    let prequel_total = prequel_total.unwrap_or(0);
    let (Some(&min), Some(&max)) = (bare.iter().min(), bare.iter().max()) else {
        // Nothing to look at yet: a sequel is more often numbered on than restarted when the
        // numbers carry no season, and the question is asked either way.
        return prequel_total;
    };
    // Numbers that start past the earlier seasons and fit this one once they are taken off are
    // numbered straight through, even when the feed has only the latest few.
    if prequel_total > 0 && min > prequel_total && max - prequel_total <= episode_count {
        return prequel_total;
    }
    if max <= episode_count {
        return 0;
    }
    min.saturating_sub(1)
}

// An episode typed into a command, read as a release number when it can only be one. A number
// inside `1..=episode_count` keeps the meaning it has always had, so nothing that worked before
// changes; only a number no episode of this season has is translated.
pub fn typed_episode(typed: u32, episode_count: u32, offset: u32) -> Option<u32> {
    if offset == 0 || (1..=episode_count).contains(&typed) {
        return None;
    }
    let episode = typed.checked_sub(offset)?;
    (1..=episode_count).contains(&episode).then_some(episode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(number: u32) -> ReleaseNumber {
        ReleaseNumber { number, season: None, version: 1 }
    }

    #[test]
    fn titles_read_as_the_episode_they_carry() {
        let cases = [
            ("[SubsPlease] Show - 64 (1080p) [ABCD1234].mkv", 64, None, 1),
            ("[SubsPlease] Show S2 - 05 (1080p) [ABCD1234].mkv", 5, Some(2), 1),
            ("[Group] Show S02E05 1080p WEB", 5, Some(2), 1),
            ("[Erai-raws] Show 2nd Season - 03 [1080p]", 3, Some(2), 1),
            ("[Group] Show Season 3 - 07v2 [1080p]", 7, Some(3), 2),
            ("[Group] 86 - Eighty Six - 05 [1080p]", 5, None, 1),
            ("[Group] Mob Psycho 100 - 1100 [1080p]", 1100, None, 1),
            ("[Group] Show - 12v2 [720p]", 12, None, 2),
            ("Show EP07 1080p", 7, None, 1),
        ];
        for (title, number, season, version) in cases {
            assert_eq!(
                release_number(title),
                Some(ReleaseNumber { number, season, version }),
                "{}",
                title
            );
        }
    }

    #[test]
    fn titles_that_are_not_one_episode_read_as_nothing() {
        for title in [
            "[Group] Show - 01-12 [1080p]",
            "[Group] Show (01 ~ 12) [1080p]",
            "[Group] Show - 01 ~ 12",
            "[Group] Show [Batch] [1080p]",
            "[Group] Show - 12.5 [1080p]",
            "[Group] Show The Movie [1080p]",
            "[Group] Show Complete Series",
        ] {
            assert_eq!(release_number(title), None, "{}", title);
        }
    }

    #[test]
    fn an_offset_maps_absolute_numbers_onto_the_season() {
        assert_eq!(map_release(&bare(64), 2, 12, 63), Some(1));
        assert_eq!(map_release(&bare(75), 2, 12, 63), Some(12));
        assert_eq!(map_release(&bare(76), 2, 12, 63), None);
        assert_eq!(map_release(&bare(63), 2, 12, 63), None);
        assert_eq!(map_release(&bare(3), 1, 12, 0), Some(3));
    }

    #[test]
    fn a_title_naming_its_season_skips_the_offset() {
        let named = ReleaseNumber { number: 5, season: Some(2), version: 1 };
        assert_eq!(map_release(&named, 2, 12, 63), Some(5));
        assert_eq!(map_release(&named, 3, 12, 0), None);
    }

    #[test]
    fn the_feed_season_is_the_one_every_title_agrees_on() {
        let s2 = ReleaseNumber { number: 1, season: Some(2), version: 1 };
        let s3 = ReleaseNumber { number: 1, season: Some(3), version: 1 };
        assert_eq!(feed_season(&[s2, s2, bare(4)], 1), 2);
        assert_eq!(feed_season(&[s2, s3], 1), 1);
        assert_eq!(feed_season(&[bare(4)], 1), 1);
    }

    #[test]
    fn the_guess_prefers_the_earlier_seasons_when_the_numbers_fit_after_them() {
        // Straight through from a 63-episode first season.
        assert_eq!(guess_offset(&[64, 65, 66], 12, Some(63)), 63);
        // Only the latest few are in the feed; the prequel count still places them.
        assert_eq!(guess_offset(&[70, 71], 12, Some(63)), 63);
        // A second season numbered from 1 again.
        assert_eq!(guess_offset(&[1, 2, 3], 12, Some(12)), 0);
        // No MyAnimeList answer: the lowest number seen is episode 1.
        assert_eq!(guess_offset(&[64, 65], 12, None), 63);
        // Numbers that fit already are left alone.
        assert_eq!(guess_offset(&[4, 5], 12, None), 0);
        // An empty feed falls back on the prequel count.
        assert_eq!(guess_offset(&[], 12, Some(24)), 24);
        assert_eq!(guess_offset(&[], 12, None), 0);
    }

    #[test]
    fn a_typed_episode_is_translated_only_when_it_can_only_be_a_release() {
        assert_eq!(typed_episode(64, 12, 63), Some(1));
        assert_eq!(typed_episode(5, 12, 63), None);
        assert_eq!(typed_episode(80, 12, 63), None);
        assert_eq!(typed_episode(64, 12, 0), None);
        // Overlapping ranges: an in-range number keeps its old meaning.
        assert_eq!(typed_episode(13, 24, 12), None);
        assert_eq!(typed_episode(30, 24, 12), Some(18));
    }

    #[test]
    fn a_plain_search_becomes_a_nyaa_feed_and_a_nyaa_page_its_own_feed() {
        let search = feed_url("[SubsPlease] Show 1080p").unwrap();
        assert!(search.starts_with("https://nyaa.si/?page=rss&q=%5BSubsPlease%5D+Show+1080p"), "{}", search);
        assert_eq!(
            feed_url("https://nyaa.si/?f=0&c=1_2&q=show").unwrap(),
            "https://nyaa.si/?page=rss&f=0&c=1_2&q=show"
        );
        assert_eq!(
            feed_url("https://nyaa.si/?page=rss&q=show").unwrap(),
            "https://nyaa.si/?page=rss&q=show"
        );
        assert_eq!(
            feed_url("https://example.org/feed.xml").unwrap(),
            "https://example.org/feed.xml"
        );
        assert!(feed_url("  ").is_err());
    }

    #[test]
    fn a_nyaa_feed_parses_into_items_keyed_by_their_guid() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<rss xmlns:nyaa="https://nyaa.si/xmlns/nyaa" version="2.0"><channel>
<item>
  <title>[SubsPlease] Show - 65 (1080p) [AAAA].mkv</title>
  <link>https://nyaa.si/download/200.torrent</link>
  <guid isPermaLink="true">https://nyaa.si/view/200</guid>
</item>
<item>
  <title><![CDATA[[Group] Tom &amp; Jerry - 64]]></title>
  <link>https://nyaa.si/download/199.torrent</link>
  <guid isPermaLink="true">https://nyaa.si/view/199</guid>
</item>
<item><title>no link</title></item>
</channel></rss>"#;
        let items = parse_feed(xml);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "[SubsPlease] Show - 65 (1080p) [AAAA].mkv");
        assert_eq!(items[0].link, "https://nyaa.si/download/200.torrent");
        assert_eq!(items[1].title, "[Group] Tom & Jerry - 64");
        assert_eq!(items[0].key, short_key("https://nyaa.si/view/200"));
        assert_eq!(items[0].key.len(), 10);
        assert_ne!(items[0].key, items[1].key);
    }

    #[test]
    fn seen_and_pending_stay_bounded_and_unique() {
        let mut config = WatchConfig::default();
        for n in 0..(SEEN_LIMIT + 5) {
            config.mark_seen(&n.to_string());
        }
        config.mark_seen("10");
        assert_eq!(config.seen.len(), SEEN_LIMIT);
        assert!(!config.has_seen("0"));
        assert!(config.has_seen(&(SEEN_LIMIT + 4).to_string()));

        let release = |key: &str| PendingRelease {
            key: key.to_string(),
            title: String::new(),
            link: String::new(),
            number: 1,
        };
        config.add_pending(release("a"));
        config.add_pending(release("a"));
        assert_eq!(config.pending.len(), 1);
        assert_eq!(config.take_pending("a").map(|p| p.key), Some("a".to_string()));
        assert!(config.take_pending("a").is_none());
    }

    #[test]
    fn a_config_survives_a_toml_round_trip() {
        let mut config = WatchConfig {
            mal_id: 1,
            feed: "Show".to_string(),
            feed_url: "https://nyaa.si/?page=rss&q=Show".to_string(),
            offset: 63,
            season: 2,
            confirmed: true,
            created_by: 5,
            setup_message: Some(9),
            ..Default::default()
        };
        config.mark_seen("abc");
        config.add_pending(PendingRelease {
            key: "k".to_string(),
            title: "t".to_string(),
            link: "https://nyaa.si/view/1".to_string(),
            number: 80,
        });
        let text = toml::to_string(&config).unwrap();
        assert_eq!(toml::from_str::<WatchConfig>(&text).unwrap(), config);
    }
}
