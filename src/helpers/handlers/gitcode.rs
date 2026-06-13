use super::*;

pub async fn handle_gitcode(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
    preset: Preset,
) -> Option<Job> {
    let subtitle_url = required_trimmed_option(ctx, command, "subtitle_url", "subtitle_url").await?;

    let normalized = github_blob_to_raw(&subtitle_url);
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to build HTTP client: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return None;
        }
    };

    let attachment_bytes = match client.get(&normalized).send().await {
        Ok(resp) => match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                command.create_response(ctx, CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(format!("Failed to fetch subtitle: {}", e))
                        .ephemeral(true)
                )).await.ok();
                return None;
            }
        },
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to fetch subtitle: {}", e))
                    .ephemeral(true)
            )).await.ok();
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
        preset,
        nyaaise(&torrent_url),
        attachment_bytes,
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    ))
}
