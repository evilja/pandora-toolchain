use super::*;

pub async fn handle_readmebase(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/readmebase").await {
        Some(id) => id,
        None => return,
    };

    let attachment = match option_attachment(command, "file") {
        Some(a) => a,
        None => {
            command_error(ctx, command, "Error: `file` attachment is required.").await;
            return;
        }
    };

    let attachment_bytes = match attachment.download().await {
        Ok(b) => b,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to download attachment: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let dir = std::path::PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string());
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to create config dir: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let path = dir.join("base.md");
    if let Err(e) = tokio::fs::write(&path, &attachment_bytes).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to write base.md: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Set `base.md` for server `{}` ({} bytes, from `{}`).",
                server_id, attachment_bytes.len(), attachment.filename))
            .ephemeral(true)
    )).await.ok();
}
