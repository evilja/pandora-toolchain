use super::*;
use pandora_toolchain::libkagami::core::SubstationAlpha;

pub async fn handle_touchwatermark(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let server_id = match command_server_id(ctx, command, "/touchwatermark").await {
        Some(id) => id,
        None => return,
    };
    let attachment = match option_attachment(command, "watermark") {
        Some(attachment) => attachment,
        None => {
            command_error(ctx, command, "Error: `watermark` attachment is required.").await;
            return;
        }
    };
    if !attachment.filename.to_ascii_lowercase().ends_with(".ass") {
        command_error(ctx, command, "Error: `watermark` must be an ASS file.").await;
        return;
    }
    let bytes = match attachment.download().await {
        Ok(bytes) => bytes,
        Err(e) => {
            command_error(ctx, command, format!("Failed to download watermark: {}", e)).await;
            return;
        }
    };
    match save_watermark_upload(server_id, command.id.get(), &attachment.filename, bytes).await {
        Ok((all, precise, default_precise)) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .embed(success_embed(command, COMMAND_UPDATED).description(format!(
                        "Saved server watermark: {} `[all]`, {} `[precise]`, {} default-precise Dialogue event(s).",
                        all, precise, default_precise
                    ))).ephemeral(true)
            )).await.ok();
        }
        Err(error) => command_error(ctx, command, error).await,
    }
}

pub(super) async fn save_watermark_upload(
    server_id: u64, operation: u64, filename: &str, bytes: Vec<u8>,
) -> Result<(usize, usize, usize), String> {
    if !filename.to_ascii_lowercase().ends_with(".ass") { return Err("Watermark must be an ASS file".into()); }
    std::str::from_utf8(&bytes).map_err(|_| "Watermark must be UTF-8".to_string())?;
    let temp = std::env::temp_dir().join(format!("pandora_watermark_{operation}.ass"));
    tokio::fs::write(&temp, &bytes).await.map_err(|e| e.to_string())?;
    let script = SubstationAlpha::load(temp.clone(), true).await;
    tokio::fs::remove_file(&temp).await.ok();
    if script.events.is_empty() { return Err("Watermark contains no Dialogue events".into()); }
    let (mut all, mut precise, mut default_precise) = (0, 0, 0);
    for event in &script.events {
        match event.effect.trim().to_ascii_lowercase().as_str() {
            "[all]" => all += 1,
            "[precise]" => precise += 1,
            _ => default_precise += 1,
        }
    }
    let dir = PathBuf::from("DB").join("config").join(server_id.to_string());
    tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
    tokio::fs::write(dir.join("watermark.ass"), bytes).await.map_err(|e| e.to_string())?;
    Ok((all, precise, default_precise))
}
