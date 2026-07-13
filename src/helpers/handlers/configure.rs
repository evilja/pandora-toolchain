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

    let existing_meta = std::fs::read_to_string(dir.join("meta.pandora")).unwrap_or_default();
    let existing_lines: Vec<&str> = existing_meta.lines().collect();
    let existing_api_key = existing_lines.get(3).copied().unwrap_or("").to_string();
    let existing_gdrive_client_id = existing_lines.get(4).copied().unwrap_or("").to_string();
    let existing_gdrive_client_secret = existing_lines.get(5).copied().unwrap_or("").to_string();
    let existing_gdrive_refresh_token = existing_lines.get(6).copied().unwrap_or("").to_string();
    let existing_gdrive_folder_id = existing_lines.get(7).copied().unwrap_or("").to_string();
    let existing_wrap_style = existing_lines.get(8).copied().unwrap_or("").to_string();
    let existing_local_gdrive = existing_lines.get(9).copied().unwrap_or("true").to_string();
    let existing_gdrive_anon_folder_id = existing_lines.get(10).copied().unwrap_or("").to_string();
    let existing_preset = existing_lines.get(11).copied().unwrap_or("standard").to_string();
    let existing_concat = existing_lines.get(12).copied().unwrap_or("").to_string();

    let wrap_style = match option_str(command, "wrapstyle").map(str::trim) {
        Some("dont_touch") | Some("keep") | Some("-") => String::new(),
        Some(v) if matches!(v, "0" | "1" | "2" | "3") => v.to_string(),
        Some(other) => {
            command_error(ctx, command, format!("Error: wrapstyle `{}` must be dont_touch, 0, 1, 2, or 3", other)).await;
            return;
        }
        None => existing_wrap_style,
    };

    let new_api_key = option_str(command, "api_key")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing_api_key)
        .to_string();
    let gdrive_client_id = option_str(command, "gdrive_client_id")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing_gdrive_client_id)
        .to_string();
    let gdrive_client_secret = option_str(command, "gdrive_client_secret")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing_gdrive_client_secret)
        .to_string();
    let gdrive_refresh_token = option_str(command, "gdrive_refresh_token")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing_gdrive_refresh_token)
        .to_string();
    let gdrive_folder_id = option_str(command, "gdrive_folder_id")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing_gdrive_folder_id)
        .to_string();
    let gdrive_anon_folder_id = option_str(command, "gdrive_anon_folder_id")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing_gdrive_anon_folder_id)
        .to_string();
    let gdrive_auth_parts = [&gdrive_client_id, &gdrive_client_secret, &gdrive_refresh_token];
    let any_gdrive = gdrive_auth_parts.iter().any(|s| !s.is_empty())
        || !gdrive_folder_id.is_empty()
        || !gdrive_anon_folder_id.is_empty();
    if any_gdrive
        && (gdrive_auth_parts.iter().any(|s| s.is_empty())
            || (gdrive_folder_id.is_empty() && gdrive_anon_folder_id.is_empty()))
    {
        command_error(ctx, command, "Error: Google Drive config requires client id, client secret, refresh token, and at least one folder id.").await;
        return;
    }

    let body = format!("{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n", language, forgejo, command.channel_id.get(), new_api_key, gdrive_client_id, gdrive_client_secret, gdrive_refresh_token, gdrive_folder_id, wrap_style, existing_local_gdrive, gdrive_anon_folder_id, existing_preset, existing_concat);
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
    let gdrive_display = if gdrive_client_id.is_empty() && gdrive_client_secret.is_empty() && gdrive_refresh_token.is_empty() && gdrive_folder_id.is_empty() && gdrive_anon_folder_id.is_empty() { "(unset)".to_string() } else { "(set)".to_string() };
    let gdrive_anon_display = if gdrive_anon_folder_id.is_empty() { "(unset)".to_string() } else { "(set)".to_string() };
    let wrap_display = if wrap_style.is_empty() { "dont_touch".to_string() } else { wrap_style.clone() };
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Configured server `{}` — language: {}, forgejo: {}, forgejo api_key: {}, gdrive: {}, gdrive_anon_folder_id: {}, wrapstyle: {}, announcement channel: <#{}>",
                server_id, language, forgejo_display, api_key_display, gdrive_display, gdrive_anon_display, wrap_display, command.channel_id.get()))
            .ephemeral(true)
    )).await.ok();
}
