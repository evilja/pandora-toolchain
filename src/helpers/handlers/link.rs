use super::*;

use pandora_toolchain::lib::channel_link::{
    is_link_use, link_use_description, linked_channel, read_links, write_links, LINK_USES,
    LINK_USE_MERGE,
};

pub async fn handle_link(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let subcommand = subcommand_options(command)
        .map(|(name, _)| name)
        .unwrap_or("list");
    let Some(server_id) = command_server_id(ctx, command, "/link").await else {
        return;
    };
    // A link redirects the output of the work this channel's repo produces, so a channel with no
    // repo attached to it has no output to send anywhere.
    if attached_repo(ctx, command, server_id, None).await.is_none() {
        return;
    }
    let channel_id = command.channel_id.get();

    match subcommand {
        "set" => set_link(ctx, command, server_id, channel_id).await,
        "list" => list_links(ctx, command, server_id, channel_id).await,
        "clear" => clear_link(ctx, command, server_id, channel_id).await,
        other => {
            command_error(ctx, command, format!("Unknown link subcommand `{}`.", other)).await;
        }
    }
}

async fn set_link(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    server_id: u64,
    channel_id: u64,
) {
    let Some(target) = option_channel(command, "channel") else {
        command_error(ctx, command, "Error: `channel` is required.").await;
        return;
    };
    let Some(link_use) = requested_use(ctx, command).await else {
        return;
    };
    if target.get() == channel_id {
        command_error(
            ctx,
            command,
            "Error: a channel linked to itself is the channel the output already goes to.",
        )
        .await;
        return;
    }

    let mut links = read_links(server_id, channel_id).await;
    links.insert(link_use.clone(), target.get());
    if let Err(e) = write_links(server_id, channel_id, &links).await {
        command_error(ctx, command, format!("Failed to write links: {}", e)).await;
        return;
    }
    respond(
        ctx,
        command,
        COMMAND_UPDATED,
        format!(
            "<#{}> now takes {}.\nPandora needs View Channel, Send Messages and Attach Files there; without them the output stays here.",
            target.get(),
            link_use_description(&link_use).unwrap_or(&link_use),
        ),
    )
    .await;
}

async fn list_links(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    server_id: u64,
    channel_id: u64,
) {
    let links = read_links(server_id, channel_id).await;
    let mut description = String::new();
    for (link_use, note) in LINK_USES {
        let line = match links.get(*link_use) {
            Some(target) => format!("`{}` → <#{}> — {}", link_use, target, note),
            None => format!("`{}` → not linked — {}", link_use, note),
        };
        description.push_str(&line);
        description.push('\n');
    }
    respond(ctx, command, COMMAND_LIST, description.trim_end().to_string()).await;
}

async fn clear_link(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    server_id: u64,
    channel_id: u64,
) {
    let Some(link_use) = requested_use(ctx, command).await else {
        return;
    };
    let mut links = read_links(server_id, channel_id).await;
    let Some(removed) = links.remove(&link_use) else {
        command_error(
            ctx,
            command,
            format!("Error: `{}` is not linked to anything on this channel.", link_use),
        )
        .await;
        return;
    };
    if let Err(e) = write_links(server_id, channel_id, &links).await {
        command_error(ctx, command, format!("Failed to write links: {}", e)).await;
        return;
    }
    respond(
        ctx,
        command,
        COMMAND_UPDATED,
        format!("<#{}> no longer takes `{}`; it comes back here.", removed, link_use),
    )
    .await;
}

// The option is a fixed list of choices, so a value outside it can only come from a stale command
// registration — worth naming rather than silently writing a link nothing will ever read.
async fn requested_use(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) -> Option<String> {
    let requested = option_trimmed(command, "use").unwrap_or_else(|| LINK_USE_MERGE.to_string());
    if !is_link_use(&requested) {
        command_error(
            ctx,
            command,
            format!("Error: `{}` is not something a channel can be linked for.", requested),
        )
        .await;
        return None;
    }
    Some(requested)
}

// What a handler asks before sending output somewhere other than where the command was typed.
pub async fn link_target(
    server_id: u64,
    channel_id: u64,
    link_use: &str,
) -> Option<serenity::all::ChannelId> {
    linked_channel(server_id, channel_id, link_use)
        .await
        .map(serenity::all::ChannelId::new)
}

async fn respond(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    message_id: &str,
    description: String,
) {
    command
        .create_response(
            ctx,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .embed(success_embed(command, message_id).description(description))
                    .ephemeral(true),
            ),
        )
        .await
        .ok();
}
