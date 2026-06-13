use super::*;

pub async fn handle_destruct(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let server_id = match command_server_id(ctx, command, "/destruct").await {
        Some(id) => id,
        None => return,
    };
    let channel_id = command.channel_id.get();

    let (meta, owner_repo, _repo_url) = match attached_repo(ctx, command, server_id, None).await {
        Some(t) => t,
        None => return,
    };
    let anime_name = meta.name.clone().unwrap_or_default();

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

    match fg.delete_repo(&owner_repo).await {
        Ok(()) => {
            let _ = tokio::fs::remove_file(meta_path(server_id, channel_id)).await;
            let name_line = if anime_name.is_empty() {
                String::new()
            } else {
                format!(" (`{}`)", anime_name)
            };
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Deleted repo `{}`{}.\nChannel detached from this anime.", owner_repo, name_line))).await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("delete_repo failed: {}", e))).await;
        }
    }
}
