use super::*;

pub async fn handle_interaction(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
) -> Option<Job> {
    let attachment_bytes = match option_attachment(command, "subtitle") {
        Some(att) => match att.download().await {
            Ok(b) => b,
            Err(e) => {
                command_error(ctx, command, format!("Failed to download attachment: {}", e)).await;
                return None;
            }
        },
        None => {
            command_error(ctx, command, "Error: Subtitle file is required").await;
            return None;
        }
    };

    let response_msg = working_response(ctx, command, "...").await?;

    response_msg.react(ctx, '❌').await.ok();

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Encode,
        response_msg.id.get(),
        nyaaise(&torrent_url),
        attachment_bytes,
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    ))
}
