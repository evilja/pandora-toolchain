use super::*;

const CLEAR_SENTINEL: &str = "-";

fn edit_text_field(command: &serenity::all::CommandInteraction, name: &str, existing: &str) -> String {
    match option_str(command, name).map(str::trim) {
        None => existing.to_string(),
        Some(CLEAR_SENTINEL) => String::new(),
        Some(s) if s.is_empty() => existing.to_string(),
        Some(s) => s.to_string(),
    }
}

pub async fn handle_edit(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/edit").await {
        Some(id) => id,
        None => return,
    };

    let dir = std::path::PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string());

    let existing_meta = std::fs::read_to_string(dir.join("meta.pandora")).unwrap_or_default();
    if existing_meta.trim().is_empty() {
        command_error(ctx, command, "Error: this server has no config yet. Run /configure first.").await;
        return;
    }
    let existing_lines: Vec<&str> = existing_meta.lines().collect();
    let existing_language = existing_lines.get(0).copied().unwrap_or("");
    let existing_forgejo = existing_lines.get(1).copied().unwrap_or("");
    let existing_channel = existing_lines.get(2).copied().unwrap_or("");
    let existing_api_key = existing_lines.get(3).copied().unwrap_or("");
    let existing_gdrive_client_id = existing_lines.get(4).copied().unwrap_or("");
    let existing_gdrive_client_secret = existing_lines.get(5).copied().unwrap_or("");
    let existing_gdrive_refresh_token = existing_lines.get(6).copied().unwrap_or("");
    let existing_gdrive_folder_id = existing_lines.get(7).copied().unwrap_or("");
    let existing_wrap_style = existing_lines.get(8).copied().unwrap_or("");
    let existing_local_gdrive = existing_lines.get(9).copied().unwrap_or("true");

    let language = match option_str(command, "language") {
        Some(l) if matches!(l, "EN" | "TR" | "JP") => l.to_string(),
        Some(other) => {
            command_error(ctx, command, format!("Error: language `{}` is not one of EN/TR/JP", other)).await;
            return;
        }
        None => existing_language.to_string(),
    };

    let forgejo = match option_str(command, "forgejo").map(str::trim) {
        None => existing_forgejo.to_string(),
        Some(CLEAR_SENTINEL) => String::new(),
        Some(u) if u.is_empty() => existing_forgejo.to_string(),
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.trim_end_matches('/').to_string(),
        Some(other) => {
            command_error(ctx, command, format!("Error: forgejo `{}` must be an http(s) URL", other)).await;
            return;
        }
    };

    let channel = match option_bool(command, "announcement_channel") {
        Some(true) => command.channel_id.get().to_string(),
        _ => existing_channel.to_string(),
    };

    let new_api_key = edit_text_field(command, "api_key", existing_api_key);
    let gdrive_client_id = edit_text_field(command, "gdrive_client_id", existing_gdrive_client_id);
    let gdrive_client_secret = edit_text_field(command, "gdrive_client_secret", existing_gdrive_client_secret);
    let gdrive_refresh_token = edit_text_field(command, "gdrive_refresh_token", existing_gdrive_refresh_token);
    let gdrive_folder_id = edit_text_field(command, "gdrive_folder_id", existing_gdrive_folder_id);
    let wrap_style = match option_str(command, "wrapstyle").map(str::trim) {
        None => existing_wrap_style.to_string(),
        Some("dont_touch") | Some("keep") | Some(CLEAR_SENTINEL) => String::new(),
        Some(v) if matches!(v, "0" | "1" | "2" | "3") => v.to_string(),
        Some(other) => {
            command_error(ctx, command, format!("Error: wrapstyle `{}` must be dont_touch, 0, 1, 2, or 3", other)).await;
            return;
        }
    };
    let local_gdrive = option_bool(command, "local_gdrive")
        .map(|v| if v { "true" } else { "false" }.to_string())
        .unwrap_or_else(|| existing_local_gdrive.to_string());
    let gdrive_parts = [&gdrive_client_id, &gdrive_client_secret, &gdrive_refresh_token, &gdrive_folder_id];
    if gdrive_parts.iter().any(|s| !s.is_empty()) && gdrive_parts.iter().any(|s| s.is_empty()) {
        command_error(ctx, command, "Error: Google Drive config requires client id, client secret, refresh token, and folder id.").await;
        return;
    }

    let body = format!("{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n", language, forgejo, channel, new_api_key, gdrive_client_id, gdrive_client_secret, gdrive_refresh_token, gdrive_folder_id, wrap_style, local_gdrive);
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
    let gdrive_display = if gdrive_client_id.is_empty() && gdrive_client_secret.is_empty() && gdrive_refresh_token.is_empty() && gdrive_folder_id.is_empty() { "(unset)".to_string() } else { "(set)".to_string() };
    let channel_display = if channel.is_empty() { "(unset)".to_string() } else { format!("<#{}>", channel) };
    let wrap_display = if wrap_style.is_empty() { "dont_touch".to_string() } else { wrap_style.clone() };
    let local_gdrive_display = if local_gdrive == "false" { "disabled" } else { "enabled" };
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Edited server `{}` — language: {}, forgejo: {}, forgejo api_key: {}, gdrive: {}, local_gdrive: {}, wrapstyle: {}, announcement channel: {}",
                server_id, language, forgejo_display, api_key_display, gdrive_display, local_gdrive_display, wrap_display, channel_display))
            .ephemeral(true)
    )).await.ok();
}
