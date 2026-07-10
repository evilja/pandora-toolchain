use super::*;
use pandora_toolchain::libkagami::core::{
    cached_font_names, collect_font_files, load_font_name_cache, normalize_font_name,
    save_font_name_cache,
};
use serde::{Deserialize, Serialize};
use serenity::builder::CreateAutocompleteResponse;
use std::collections::{BTreeSet, HashMap, HashSet};

const DEFAULT_PREVIEW_WATERMARK_FONT: &str = "Gandhi Sans Bold";
const MAX_FONT_CHOICES: usize = 25;
const MAX_FONT_CHOICE_CHARS: usize = 100;
// Discord voids autocomplete interactions after 3s; leave headroom for the
// HTTP round trip to the response endpoint.
const AUTOCOMPLETE_SCAN_BUDGET: std::time::Duration = std::time::Duration::from_millis(2000);

#[derive(Default)]
struct FontChoiceCache {
    names: HashMap<PathBuf, BTreeSet<String>>,
    scanned: HashSet<PathBuf>,
}

fn font_choice_cache() -> &'static std::sync::RwLock<FontChoiceCache> {
    static CACHE: std::sync::OnceLock<std::sync::RwLock<FontChoiceCache>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::RwLock::new(FontChoiceCache::default()))
}

#[derive(Deserialize, Serialize)]
struct PreviewConfig {
    watermark_font: String,
}

pub async fn handle_cfont(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let server_id = match command_server_id(ctx, command, "/cfont").await {
        Some(id) => id,
        None => return,
    };
    // The font scan below can outlive Discord's 3s initial-response window;
    // acknowledge first, then deliver every reply through edit_response.
    command
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await
        .ok();

    if let Some(name) = option_trimmed(command, "font") {
        let Some(path) = find_preview_font(server_id, &name).await else {
            respond(
                ctx,
                command,
                format!(
                    "Font `{}` was not found in `DB/fontconfig/{}` or `DB/fontconfig/global`.",
                    name, server_id
                ),
            )
            .await;
            return;
        };
        if let Err(e) = write_preview_font(server_id, &name).await {
            respond(ctx, command, format!("Failed to save preview font: {}", e)).await;
            return;
        }
        respond(
            ctx,
            command,
            format!(
                "Preview watermark font set to `{}` (`{}`).",
                name,
                path.display()
            ),
        )
        .await;
        return;
    }

    let configured = read_preview_watermark_font_name(server_id);
    let resolved = resolve_preview_watermark_font_path(server_id).await;
    let resolved = resolved
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "embedded fallback".to_string());
    respond(
        ctx,
        command,
        format!(
            "Preview watermark font: `{}`\nResolved: `{}`",
            configured, resolved
        ),
    )
    .await;
}

async fn respond(ctx: &Context, command: &serenity::all::CommandInteraction, content: String) {
    command
        .edit_response(ctx, EditInteractionResponse::new().content(content))
        .await
        .ok();
}

pub async fn handle_cfont_autocomplete(
    ctx: &Context,
    interaction: &serenity::all::CommandInteraction,
) {
    let partial = interaction
        .data
        .autocomplete()
        .filter(|option| option.name == "font")
        .map(|option| option.value.to_string())
        .unwrap_or_default();
    let roots = match interaction.guild_id {
        Some(guild_id) => preview_font_roots(guild_id.get()),
        None => vec![PathBuf::from("DB").join("fontconfig").join("global")],
    };
    let names = if let Some(names) = cached_font_family_names(&roots) {
        names
    } else {
        let scan = tokio::task::spawn_blocking(move || refresh_font_family_names(&roots));
        match tokio::time::timeout(AUTOCOMPLETE_SCAN_BUDGET, scan).await {
            Ok(joined) => joined.unwrap_or_default(),
            Err(_) => {
                // Cold cache on a large font dir. Answer empty within the deadline;
                // the dropped scan keeps running on the blocking pool and publishes
                // a complete family index for subsequent keystrokes.
                eprintln!("[cfont] font scan exceeded the autocomplete budget; responding empty");
                BTreeSet::new()
            }
        }
    };
    let mut response = CreateAutocompleteResponse::new();
    for name in filter_font_choices(&names, &partial) {
        response = response.add_string_choice(name.clone(), name);
    }
    if let Err(e) = interaction
        .create_response(ctx, CreateInteractionResponse::Autocomplete(response))
        .await
    {
        eprintln!("[cfont] autocomplete response failed: {}", e);
    }
}

// Walks every DB fontconfig dir once so the per-file name cache is hot before
// the first autocomplete interaction has to race Discord's 3s deadline. The
// cache is seeded from and saved back to an on-disk index, so only fonts that
// changed since the previous boot are actually re-parsed.
pub fn warm_font_name_cache() {
    tokio::task::spawn_blocking(|| {
        let started = std::time::Instant::now();
        let index = font_name_index_path();
        let seeded = load_font_name_cache(&index);
        let roots = font_choice_roots();
        let files = roots
            .iter()
            .map(|root| refresh_font_family_names_for_root(root).1)
            .sum::<usize>();
        match save_font_name_cache(&index) {
            Ok(saved) => println!(
                "[fonts] font name cache ready: {} file(s), {} seeded from index, {} saved, {:.1?}",
                files,
                seeded,
                saved,
                started.elapsed()
            ),
            Err(e) => eprintln!("[fonts] font name index save failed: {}", e),
        }
    });
}

// Refreshes the affected bucket after /font extracts new files. This keeps
// autocomplete disk-free without requiring a bot restart to see new fonts.
pub async fn refresh_font_name_choices(server_id: u64) {
    let root = PathBuf::from("DB")
        .join("fontconfig")
        .join(server_id.to_string());
    if tokio::task::spawn_blocking(move || refresh_font_family_names_for_root(&root))
        .await
        .is_err()
    {
        eprintln!("[cfont] font choice refresh task failed for server {}", server_id);
    }
}

fn font_name_index_path() -> PathBuf {
    PathBuf::from("DB").join("cache").join("font_names.json")
}

fn font_choice_roots() -> Vec<PathBuf> {
    let root = PathBuf::from("DB").join("fontconfig");
    let mut roots = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                roots.push(entry.path());
            }
        }
    }
    roots.sort();
    roots
}

fn cached_font_family_names(roots: &[PathBuf]) -> Option<BTreeSet<String>> {
    let cache = font_choice_cache().read().unwrap();
    if roots.iter().any(|root| !cache.scanned.contains(root)) {
        return None;
    }
    let mut names = BTreeSet::new();
    for root in roots {
        if let Some(root_names) = cache.names.get(root) {
            names.extend(root_names.iter().cloned());
        }
    }
    Some(names)
}

fn refresh_font_family_names(roots: &[PathBuf]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for root in roots {
        if let Some(cached) = cached_font_family_names(std::slice::from_ref(root)) {
            names.extend(cached);
        } else {
            names.extend(refresh_font_family_names_for_root(root).0);
        }
    }
    names
}

fn refresh_font_family_names_for_root(root: &Path) -> (BTreeSet<String>, usize) {
    let mut files = Vec::new();
    collect_font_files(root, &mut files);
    let mut names = BTreeSet::new();
    for path in &files {
        for name in cached_font_names(path) {
            let name = name.trim();
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
    }
    let mut cache = font_choice_cache().write().unwrap();
    cache.names.insert(root.to_path_buf(), names.clone());
    cache.scanned.insert(root.to_path_buf());
    (names, files.len())
}

fn filter_font_choices(names: &BTreeSet<String>, partial: &str) -> Vec<String> {
    // Every typed word must appear somewhere in the normalized name, so
    // "gandhi bold" still finds "Gandhi Sans Bold".
    let needles: Vec<String> = partial
        .split_whitespace()
        .map(normalize_font_name)
        .filter(|needle| !needle.is_empty())
        .collect();
    names
        .iter()
        .filter(|name| {
            let haystack = normalize_font_name(name);
            needles.iter().all(|needle| haystack.contains(needle.as_str()))
        })
        .take(MAX_FONT_CHOICES)
        .map(|name| name.chars().take(MAX_FONT_CHOICE_CHARS).collect())
        .collect()
}

pub async fn resolve_preview_watermark_font_path(server_id: u64) -> Option<PathBuf> {
    let configured = read_preview_watermark_font_name(server_id);
    if let Some(path) = find_preview_font(server_id, &configured).await {
        return Some(path);
    }
    if configured != DEFAULT_PREVIEW_WATERMARK_FONT {
        return find_preview_font(server_id, DEFAULT_PREVIEW_WATERMARK_FONT).await;
    }
    None
}

fn read_preview_watermark_font_name(server_id: u64) -> String {
    let path = preview_config_path(server_id);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| toml::from_str::<PreviewConfig>(&raw).ok())
        .map(|cfg| cfg.watermark_font.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| DEFAULT_PREVIEW_WATERMARK_FONT.to_string())
}

async fn write_preview_font(server_id: u64, name: &str) -> Result<(), String> {
    let path = preview_config_path(server_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
    }
    let body = toml::to_string(&PreviewConfig {
        watermark_font: name.to_string(),
    })
    .map_err(|e| e.to_string())?;
    tokio::fs::write(path, body).await.map_err(|e| e.to_string())
}

async fn find_preview_font(server_id: u64, name: &str) -> Option<PathBuf> {
    let names = vec![name.to_string()];
    let roots = preview_font_roots(server_id);
    tokio::task::spawn_blocking(move || find_fonts_with_roots(&names, &roots))
        .await
        .ok()
        .and_then(|paths| paths.into_iter().next())
}

fn preview_font_roots(server_id: u64) -> Vec<PathBuf> {
    vec![
        PathBuf::from("DB")
            .join("fontconfig")
            .join(server_id.to_string()),
        PathBuf::from("DB").join("fontconfig").join("global"),
    ]
}

fn preview_config_path(server_id: u64) -> PathBuf {
    PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join("preview.toml")
}

#[cfg(test)]
mod tests {
    use super::{filter_font_choices, MAX_FONT_CHOICES};
    use std::collections::BTreeSet;

    fn names(entries: &[&str]) -> BTreeSet<String> {
        entries.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn filter_matches_case_and_space_insensitive_words() {
        let set = names(&["Gandhi Sans", "Gandhi Sans Bold", "Liberation Mono"]);
        assert_eq!(
            filter_font_choices(&set, "gandhi bold"),
            vec!["Gandhi Sans Bold".to_string()]
        );
        assert_eq!(
            filter_font_choices(&set, "GANDHI sans"),
            vec!["Gandhi Sans".to_string(), "Gandhi Sans Bold".to_string()]
        );
        assert_eq!(
            filter_font_choices(&set, "ion mo"),
            vec!["Liberation Mono".to_string()]
        );
        assert_eq!(filter_font_choices(&set, "comic"), Vec::<String>::new());
    }

    #[test]
    fn filter_returns_alphabetical_head_for_empty_partial() {
        let set = names(&["B Font", "A Font", "C Font"]);
        assert_eq!(
            filter_font_choices(&set, ""),
            vec![
                "A Font".to_string(),
                "B Font".to_string(),
                "C Font".to_string()
            ]
        );
    }

    #[test]
    fn filter_caps_choices_and_truncates_long_names() {
        let mut set = BTreeSet::new();
        for idx in 0..40 {
            set.insert(format!("Font {:02}", idx));
        }
        assert_eq!(filter_font_choices(&set, "font").len(), MAX_FONT_CHOICES);

        let long = names(&[&"x".repeat(140)]);
        let choices = filter_font_choices(&long, "");
        assert_eq!(choices[0].chars().count(), 100);
    }
}
