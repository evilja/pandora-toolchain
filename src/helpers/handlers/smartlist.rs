use super::*;

use pandora_toolchain::lib::db::core::JobDb;
use std::collections::BTreeMap;

// Discord refuses a message body over 2000 characters and a two-cour anime with four hosts an
// episode is well past that, so the list is sent as as many messages as it takes.
const MESSAGE_LIMIT: usize = 1900;

pub async fn handle_smartlist(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let server_id = match command_server_id(ctx, command, "/smartlist").await {
        Some(id) => id,
        None => return,
    };
    // Only for the attachment, not for a repo read: the links come from the job db, but "this
    // channel's anime" is what the attachment means and an unattached channel has no answer.
    if attached_repo(ctx, command, server_id, None).await.is_none() {
        return;
    }
    let db = match JobDb::new().await {
        Ok(db) => db,
        Err(e) => {
            command_error(ctx, command, format!("Database error: {}", e)).await;
            return;
        }
    };
    let rows = match db.get_uploaded_jobs_by_channel(command.channel_id.get()).await {
        Ok(rows) => rows,
        Err(e) => {
            command_error(ctx, command, format!("Database error: {}", e)).await;
            return;
        }
    };

    // Newest first out of the query, so the first row an episode appears in is the encode that
    // replaced the earlier ones, and a re-encode lists the episode once rather than twice.
    let mut latest: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for row in &rows {
        let Some(episode) = row.episode_number() else {
            continue;
        };
        let links = row.uploaded_link_urls();
        if links.is_empty() {
            continue;
        }
        latest.entry(episode).or_insert(links);
    }
    if latest.is_empty() {
        command_error(
            ctx,
            command,
            "Error: no uploaded episode of this anime is recorded yet.",
        )
        .await;
        return;
    }

    let mut messages: Vec<String> = Vec::new();
    for (episode, links) in &latest {
        let block = format!("{}:\n{}", episode, links.join("\n"));
        match messages.last_mut() {
            Some(current) if current.len() + block.len() + 2 <= MESSAGE_LIMIT => {
                current.push_str("\n\n");
                current.push_str(&block);
            }
            _ => messages.push(block),
        }
    }

    let mut messages = messages.into_iter();
    let Some(first) = messages.next() else {
        return;
    };
    // Plain text rather than an embed, and public rather than ephemeral: the list exists to be
    // read and copied out of the channel it was asked for in.
    if let Err(error) = command
        .create_response(
            ctx,
            CreateInteractionResponse::Message(CreateInteractionResponseMessage::new().content(first)),
        )
        .await
    {
        report_interaction_failure("smartlist reply", command, &error);
        return;
    }
    for message in messages {
        if let Err(error) = command
            .create_followup(
                ctx,
                serenity::all::CreateInteractionResponseFollowup::new().content(message),
            )
            .await
        {
            report_interaction_failure("smartlist page", command, &error);
            return;
        }
    }
}
