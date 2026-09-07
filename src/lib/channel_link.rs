// A channel linked to an attached channel, and what the link is for. The attached channel is where
// the work is done; the linked one is where a particular piece of that work's output is wanted
// instead. Today there is one such piece — the release ASS `/merge` answers with when
// `/edit merge_release_only` is on — and a group usually wants that in the channel its members
// read rather than in the one its editors type commands in.
//
// A link belongs to the channel it was set on, so it lives beside that channel's `meta.toml` and,
// like the channel's `/attribute` styles, outlives a `/detach` — re-attaching the same channel
// finds the link it had. `/link clear` is what takes one away.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::lib::attribute::channel_dir;

pub const LINK_USE_MERGE: &str = "merge";

// Every use a link can be set for, with the sentence `/link list` prints beside it. Adding a use
// means adding it here and reading it wherever that output is produced.
pub const LINK_USES: &[(&str, &str)] = &[(
    LINK_USE_MERGE,
    "the release ASS `/merge` sends when `/edit merge_release_only` is on",
)];

pub fn is_link_use(name: &str) -> bool {
    LINK_USES.iter().any(|(link_use, _)| *link_use == name)
}

pub fn link_use_description(name: &str) -> Option<&'static str> {
    LINK_USES
        .iter()
        .find(|(link_use, _)| *link_use == name)
        .map(|(_, description)| *description)
}

pub fn links_path(server_id: u64, channel_id: u64) -> PathBuf {
    channel_dir(server_id, channel_id).join("links.json")
}

// Ids are stored as strings. A Discord snowflake is past what a JSON number survives in every
// reader that touches these files — the web console's included — and this one is read by hand
// often enough for that to matter.
pub async fn read_links(server_id: u64, channel_id: u64) -> BTreeMap<String, u64> {
    let Ok(raw) = tokio::fs::read_to_string(links_path(server_id, channel_id)).await else {
        return BTreeMap::new();
    };
    let parsed: BTreeMap<String, String> = serde_json::from_str(&raw).unwrap_or_default();
    parsed
        .into_iter()
        .filter_map(|(link_use, id)| id.trim().parse::<u64>().ok().map(|id| (link_use, id)))
        .collect()
}

pub async fn write_links(
    server_id: u64,
    channel_id: u64,
    links: &BTreeMap<String, u64>,
) -> Result<(), String> {
    let path = links_path(server_id, channel_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let stored: BTreeMap<&str, String> = links
        .iter()
        .map(|(link_use, id)| (link_use.as_str(), id.to_string()))
        .collect();
    let raw = serde_json::to_string_pretty(&stored).map_err(|e| e.to_string())?;
    tokio::fs::write(&path, raw).await.map_err(|e| e.to_string())
}

pub async fn linked_channel(server_id: u64, channel_id: u64, link_use: &str) -> Option<u64> {
    read_links(server_id, channel_id).await.get(link_use).copied()
}
