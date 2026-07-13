use super::*;

pub async fn handle_backup(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
) -> Option<Job> {
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Backup process will begin shortly after...")
    )).await.ok();

    let response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Backup,
        response_msg.id.get(),
        nyaaise(&torrent_url),
        vec![],
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    ))
}
