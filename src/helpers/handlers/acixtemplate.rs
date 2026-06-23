use super::*;

pub async fn handle_acixtemplate(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/acixtemplate").await {
        Some(id) => id,
        None => return,
    };
    let channel_id = command.channel_id.get();

    let template = match positive_u32_option(ctx, command, "template").await {
        Some(v) => v as i64,
        None => return,
    };

    let mut meta = read_channel_meta(server_id, channel_id);
    if meta.mal_id.is_none() {
        command_error(ctx, command, "Error: attach an anime to this channel first (/attach or /init).").await;
        return;
    }
    meta.acix_template = Some(template);

    if let Err(e) = write_channel_meta(server_id, channel_id, &meta).await {
        command_error(ctx, command, format!("Failed to save channel meta: {}", e)).await;
        return;
    }

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("AnimeciX fansub template for this channel set to `{}`.", template))
            .ephemeral(true)
    )).await.ok();
}
