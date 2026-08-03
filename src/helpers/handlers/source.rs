use super::*;

pub async fn handle_source(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let episode = match positive_u32_option(ctx, command, "episode").await {
        Some(n) => n,
        None => return,
    };
    let link = match option_trimmed(command, "link") {
        Some(s) => s,
        None => {
            command_error(ctx, command, "Error: `link` is required.").await;
            return;
        }
    };
    let server_id = match command_server_id(ctx, command, "/source").await {
        Some(id) => id,
        None => return,
    };
    let (_meta, owner_repo, repo_url) = match attached_repo(ctx, command, server_id, Some(episode)).await {
        Some(t) => t,
        None => return,
    };
    let (forgejo_base, api_key) = match forgejo_config(ctx, command, server_id).await {
        Some(t) => t,
        None => return,
    };
    let mut response_msg = match working_response(ctx, command, "Working…").await {
        Some(m) => m,
        None => return,
    };

    let fg = match Forgejo::new(forgejo_base, api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Forgejo init failed: {}", e))).await;
            return;
        }
    };

    let folder = pad2(episode);
    let source_path = format!("{}/SOURCE.md", folder);
    let source_content = format!("# {}\n", source_link(&link));
    let source_b64 = base64_encode(&source_content);
    match fg.upsert_file(&owner_repo, &source_path, &source_b64, "Set source link").await {
        Ok(()) => {
            remove_gitkeep_for_path(&fg, &owner_repo, &source_path).await;
            let source_display = if link.starts_with("magnet:") {
                command_message(command, VALUE_MAGNET_HIDDEN)
            } else {
                source_link(&link)
            };
            let embed = success_embed(command, COMMAND_SOURCE_UPDATED)
                .field(
                    command_message(command, FIELD_REPO),
                    format!("[{}]({})", owner_repo, repo_url),
                    true,
                )
                .field(
                    command_message(command, FIELD_EPISODE),
                    format!("`{}`", episode),
                    true,
                )
                .field(
                    command_message(command, FIELD_PATH),
                    format!("`{}`", source_path),
                    false,
                )
                .field(
                    command_message(command, FIELD_SOURCE),
                    source_display,
                    false,
                );
            edit_response_embed(ctx, &mut response_msg, embed).await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to write `{}`: {}", source_path, e))).await;
        }
    }
}
