use super::*;

pub async fn handle_configure(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/configure").await {
        Some(id) => id,
        None => return,
    };

    let language = match option_str(command, "language") {
        Some(l) if matches!(l, "EN" | "TR" | "JP") => l.to_string(),
        Some(other) => {
            command_error(ctx, command, format!("Error: language `{}` is not one of EN/TR/JP", other)).await;
            return;
        }
        None => {
            command_error(ctx, command, "Error: language is required").await;
            return;
        }
    };

    let forgejo = match option_str(command, "forgejo") {
        Some(u) if u.is_empty() => String::new(),
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.trim_end_matches('/').to_string(),
        Some(other) => {
            command_error(ctx, command, format!("Error: forgejo `{}` must be an http(s) URL", other)).await;
            return;
        }
        None => String::new(),
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

    let existing_api_key = std::fs::read_to_string(dir.join("meta.pandora"))
        .ok()
        .and_then(|s| s.lines().nth(3).map(str::to_string))
        .unwrap_or_default();

    let new_api_key = option_str(command, "api_key")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing_api_key)
        .to_string();

    let body = format!("{}\n{}\n{}\n{}\n", language, forgejo, command.channel_id.get(), new_api_key);
    let path = dir.join("meta.pandora");
    if let Err(e) = tokio::fs::write(&path, body).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to write meta.pandora: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let forgejo_display = if forgejo.is_empty() { "(unset)".to_string() } else { format!("`{}`", forgejo) };
    let api_key_display = if new_api_key.is_empty() { "(unset)".to_string() } else { "(set)".to_string() };
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Configured server `{}` — language: {}, forgejo: {}, forgejo api_key: {}, announcement channel: <#{}>",
                server_id, language, forgejo_display, api_key_display, command.channel_id.get()))
            .ephemeral(true)
    )).await.ok();
}
