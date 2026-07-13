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
    if let Err(e) = String::from_utf8(bytes.clone()) {
        command_error(
            ctx,
            command,
            format!("Error: watermark is not valid UTF-8: {}", e),
        )
        .await;
        return;
    }

    let temp = std::env::temp_dir().join(format!("pandora_watermark_{}.ass", command.id.get()));
    if let Err(e) = tokio::fs::write(&temp, &bytes).await {
        command_error(ctx, command, format!("Failed to prepare watermark: {}", e)).await;
        return;
    }
    let script = SubstationAlpha::load(temp.clone(), true).await;
    tokio::fs::remove_file(&temp).await.ok();
    if script.events.is_empty() {
        command_error(
            ctx,
            command,
            "Error: watermark contains no Dialogue events.",
        )
        .await;
        return;
    }

    let mut all = 0usize;
    let mut precise = 0usize;
    let mut default_precise = 0usize;
    for event in &script.events {
        match event.effect.trim().to_ascii_lowercase().as_str() {
            "[all]" => all += 1,
            "[precise]" => precise += 1,
            _ => default_precise += 1,
        }
    }

    let dir = std::path::PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string());
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        command_error(
            ctx,
            command,
            format!("Failed to create server config directory: {}", e),
        )
        .await;
        return;
    }
    if let Err(e) = tokio::fs::write(dir.join("watermark.ass"), bytes).await {
        command_error(ctx, command, format!("Failed to save watermark: {}", e)).await;
        return;
    }
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!(
                "Saved server watermark: {} `[all]`, {} `[precise]`, {} default-precise Dialogue event(s).",
                all, precise, default_precise
            ))
            .ephemeral(true)
    )).await.ok();
}
