use super::*;
use serde::{Deserialize, Serialize};

const DEFAULT_PREVIEW_WATERMARK_FONT: &str = "Gandhi Sans Bold";

#[derive(Deserialize, Serialize)]
struct PreviewConfig {
    watermark_font: String,
}

pub async fn handle_cfont(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let server_id = match command_server_id(ctx, command, "/cfont").await {
        Some(id) => id,
        None => return,
    };

    if let Some(name) = option_trimmed(command, "font") {
        let Some(path) = find_preview_font(server_id, &name).await else {
            command_error(
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
            command_error(ctx, command, format!("Failed to save preview font: {}", e)).await;
            return;
        }
        command
            .create_response(
                ctx,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(format!(
                            "Preview watermark font set to `{}` (`{}`).",
                            name,
                            path.display()
                        ))
                        .ephemeral(true),
                ),
            )
            .await
            .ok();
        return;
    }

    let configured = read_preview_watermark_font_name(server_id);
    let resolved = resolve_preview_watermark_font_path(server_id).await;
    let resolved = resolved
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "embedded fallback".to_string());
    command
        .create_response(
            ctx,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!(
                        "Preview watermark font: `{}`\nResolved: `{}`",
                        configured, resolved
                    ))
                    .ephemeral(true),
            ),
        )
        .await
        .ok();
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
