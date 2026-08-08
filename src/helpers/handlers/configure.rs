use super::*;
use pandora_toolchain::pnworker::server_config::{drive_only_from_meta, fansub_from_meta, FansubSite};

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
    let existing_drive_only = drive_only_from_meta(&existing_meta);
    let existing_fansub = |site: FansubSite| fansub_from_meta(&existing_meta, site).unwrap_or_default();

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
    let gdrive_client_id = existing_gdrive_client_id;
    let gdrive_client_secret = existing_gdrive_client_secret;
    let gdrive_refresh_token = existing_gdrive_refresh_token;
    let gdrive_folder_id = existing_gdrive_folder_id;
    let gdrive_anon_folder_id = existing_gdrive_anon_folder_id;

    let body = compose_server_meta(&ServerMetaFields {
        language: language.clone(),
        forgejo: forgejo.clone(),
        announcement_channel: command.channel_id.get().to_string(),
        api_key: new_api_key.clone(),
        gdrive_client_id,
        gdrive_client_secret,
        gdrive_refresh_token,
        gdrive_folder_id,
        wrap_style: wrap_style.clone(),
        local_gdrive: existing_local_gdrive,
        gdrive_anon_folder_id,
        preset: existing_preset,
        concat: existing_concat,
        animecix_fansub: existing_fansub(FansubSite::AnimeciX),
        drive_only: existing_drive_only.to_string(),
        openanime_fansub: existing_fansub(FansubSite::OpenAnime),
        anizm_fansub: existing_fansub(FansubSite::Anizm),
    });
    let path = dir.join("meta.pandora");
    if let Err(e) = tokio::fs::write(&path, body).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to write meta.pandora: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let set = command_message(command, VALUE_SET);
    let unset = command_message(command, VALUE_UNSET);
    let forgejo_display = if forgejo.is_empty() { unset.clone() } else { forgejo.clone() };
    let api_key_display = if new_api_key.is_empty() { unset.clone() } else { set.clone() };
    let broker_status = match LumiereClient::from_env() {
        Ok(client) => client
            .provider_status(Some(&guild_drive_profile(server_id)))
            .await
            .unwrap_or_default(),
        Err(_) => Default::default(),
    };
    let gdrive_display = if broker_status.requested_drive || broker_status.global_drive { set.clone() } else { unset.clone() };
    let gdrive_anon_display = if broker_status.requested_drive { set } else { unset };
    let wrap_display = if wrap_style.is_empty() { "dont_touch".to_string() } else { wrap_style.clone() };
    let drive_only_display = command_message(command, if existing_drive_only { VALUE_ENABLED } else { VALUE_DISABLED });
    let embed = success_embed(command, COMMAND_SERVER_CONFIGURED)
        .description(format!("Server `{}`", server_id))
        .field(command_message(command, FIELD_LANGUAGE), language, true)
        .field(command_message(command, FIELD_REPO), forgejo_display, true)
        .field(command_message(command, FIELD_API_KEY), api_key_display, true)
        .field(command_message(command, FIELD_GDRIVE), gdrive_display, true)
        .field(command_message(command, FIELD_GDRIVE_ANONYMOUS), gdrive_anon_display, true)
        .field(command_message(command, FIELD_DRIVE_ONLY), drive_only_display, true)
        .field(command_message(command, FIELD_WRAPSTYLE), wrap_display, true)
        .field(
            command_message(command, FIELD_ANNOUNCEMENT),
            format!("<#{}>", command.channel_id.get()),
            false,
        );
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .embed(embed)
            .ephemeral(true)
    )).await.ok();
}
