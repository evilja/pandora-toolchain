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

    let anime = if anime_name.is_empty() {
        command_message(command, VALUE_NOT_AVAILABLE)
    } else {
        anime_name
    };
    let embed = success_embed(command, COMMAND_CHANNEL_DETACHED)
        .description(command_message(command, COMMAND_REPO_PRESERVED))
        .field(command_message(command, FIELD_ANIME), anime, true)
        .field(
            command_message(command, FIELD_REPO),
            format!("[{}]({})", repo_url, repo_url),
            false,
        );
    edit_response_embed(ctx, &mut response_msg, embed).await;
}
