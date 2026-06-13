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
    let (_meta, owner_repo, _repo_url) = match attached_repo(ctx, command, server_id, Some(episode)).await {
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
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Wrote `{}` with:\n```\n{}\n```", source_path, source_content.trim_end()))).await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to write `{}`: {}", source_path, e))).await;
        }
    }
}
