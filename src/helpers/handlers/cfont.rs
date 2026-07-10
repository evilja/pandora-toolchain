use super::*;
use pandora_toolchain::libkagami::core::{cached_font_names, collect_font_files, normalize_font_name};
use serde::{Deserialize, Serialize};
use serenity::builder::CreateAutocompleteResponse;
use std::collections::BTreeSet;

const DEFAULT_PREVIEW_WATERMARK_FONT: &str = "Gandhi Sans Bold";
const MAX_FONT_CHOICES: usize = 25;
const MAX_FONT_CHOICE_CHARS: usize = 100;

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
    let names = tokio::task::spawn_blocking(move || collect_font_family_names(&roots))
        .await
        .unwrap_or_default();
    let mut response = CreateAutocompleteResponse::new();
    for name in filter_font_choices(&names, &partial) {
        response = response.add_string_choice(name.clone(), name);
    }
    interaction
        .create_response(ctx, CreateInteractionResponse::Autocomplete(response))
        .await
        .ok();
}

fn collect_font_family_names(roots: &[PathBuf]) -> BTreeSet<String> {
    let mut files = Vec::new();
    for root in roots {
        collect_font_files(root, &mut files);
    }
    let mut names = BTreeSet::new();
    for path in &files {
        for name in cached_font_names(path) {
            let name = name.trim();
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
    }
    names
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
