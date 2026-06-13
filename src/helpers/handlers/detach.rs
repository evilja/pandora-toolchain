use super::*;

pub async fn handle_detach(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let server_id = match command_server_id(ctx, command, "/detach").await {
        Some(id) => id,
        None => return,
    };
    let channel_id = command.channel_id.get();

    let meta = read_channel_meta(server_id, channel_id);
    if meta.repo_url.as_deref().map_or(true, str::is_empty) {
        command_error(ctx, command, "Error: this channel is not attached to an anime.").await;
        return;
    }
    let anime_name = meta.name.clone().unwrap_or_default();
    let repo_url = meta.repo_url.clone().unwrap_or_default();

    let mut response_msg = match working_response(ctx, command, "Working…").await {
        Some(m) => m,
        None => return,
    };

    let _ = tokio::fs::remove_file(meta_path(server_id, channel_id)).await;

    let name_line = if anime_name.is_empty() {
        String::new()
    } else {
        format!(" (`{}`)", anime_name)
    };
    let _ = response_msg.edit(ctx, EditMessage::new()
        .content(format!("Detached channel from{}.\nRepo `{}` is left untouched.", name_line, repo_url))).await;
}
