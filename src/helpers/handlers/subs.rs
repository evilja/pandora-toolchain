use super::*;

// `/subs` takes a source link and nothing else. A season pack has no single "the" video to extract
// from, so the job lists a torrent first and asks for an index in chat when there is a choice —
// the caller arms that with `Job::pick_file_first`, the same way `/encode do` does.
pub async fn handle_subs(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) -> Option<Job> {
    let source = required_trimmed_option(ctx, command, "torrent", "Torrent URL").await?;

    let response_msg = working_response(ctx, command, "...").await?;
    if let Err(error) = response_msg.react(ctx, '❌').await {
        // Not fatal — the job runs either way — but the ❌ is the only way to cancel it from
        // Discord, so silently not having one is worth a line.
        report_send_failure("cancel reaction", response_msg.channel_id.get(), &error);
    }

    let mut job = Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Subs,
        response_msg.id.get(),
        nyaaise(&source),
        Vec::new(),
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|guild| guild.get()),
    );
    job.display_link = Some(display_source_link(&source));
    Some(job)
}
