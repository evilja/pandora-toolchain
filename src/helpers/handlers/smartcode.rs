use super::*;

pub async fn handle_smartcode(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    intros: &IntrosConfig,
) -> Option<Job> {
    let preset = resolve_preset(command, intros);
    let mut response_msg = working_response(ctx, command, "Working…").await?;
    let result = smartcode_merge_upload(ctx, command, &mut response_msg, "/smartcode", "smartcode").await?;

    let _ = response_msg.edit(ctx, EditMessage::new().content("...")).await;

    response_msg.react(ctx, '❌').await.ok();

    let final_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        final_msg.id.get(),
        JobType::Encode,
        final_msg.id.get(),
        preset,
        nyaaise(&result.link),
        result.merged_bytes,
        ctx.clone(),
        final_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    ))
}
