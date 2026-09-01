use serenity::{
    all::{Colour, CommandDataOption, CommandDataOptionValue, Context, CreateEmbed, EditMessage, Message},
    builder::{CreateEmbedFooter, CreateInteractionResponse, CreateInteractionResponseMessage},
};

use pandora_toolchain::pnworker::messages::{
    format_message, get_message, COMMAND_WORKING, EMBED_FOOTER,
};

use super::{parse_repo_url, read_channel_meta, read_server_meta, ChannelMeta};

const PKGVER: &str = env!("CARGO_PKG_VERSION");

pub(super) fn read_credit_option(command: &serenity::all::CommandInteraction, name: &str) -> String {
    option_str(command, name)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("---")
        .to_string()
}

pub(super) fn subcommand_options(
    command: &serenity::all::CommandInteraction,
) -> Option<(&str, &[CommandDataOption])> {
    command.data.options.first().and_then(|opt| match &opt.value {
        CommandDataOptionValue::SubCommand(options) => Some((opt.name.as_str(), options.as_slice())),
        _ => None,
    })
}

fn command_options(command: &serenity::all::CommandInteraction) -> &[CommandDataOption] {
    subcommand_options(command)
        .map(|(_, options)| options)
        .unwrap_or(&command.data.options)
}

pub(super) fn option_str<'a>(command: &'a serenity::all::CommandInteraction, name: &str) -> Option<&'a str> {
    command_options(command).iter()
        .find(|opt| opt.name == name)
        .and_then(|opt| opt.value.as_str())
}

pub(super) fn option_trimmed(command: &serenity::all::CommandInteraction, name: &str) -> Option<String> {
    option_str(command, name)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

pub(super) fn option_i64(command: &serenity::all::CommandInteraction, name: &str) -> Option<i64> {
    command_options(command).iter()
        .find(|opt| opt.name == name)
        .and_then(|opt| opt.value.as_i64())
}

pub(super) fn option_f64(command: &serenity::all::CommandInteraction, name: &str) -> Option<f64> {
    command_options(command).iter()
        .find(|opt| opt.name == name)
        .and_then(|opt| opt.value.as_f64())
}

pub(super) fn option_bool(command: &serenity::all::CommandInteraction, name: &str) -> Option<bool> {
    command_options(command).iter()
        .find(|opt| opt.name == name)
        .and_then(|opt| opt.value.as_bool())
}

pub(super) fn option_attachment<'a>(
    command: &'a serenity::all::CommandInteraction,
    name: &str,
) -> Option<&'a serenity::all::Attachment> {
    command_options(command).iter()
        .find(|opt| opt.name == name)
        .and_then(|opt| opt.value.as_attachment_id())
        .and_then(|id| command.data.resolved.attachments.get(&id))
}

// Every reply to an interaction ends in `.ok()`, which is right — there is nothing a handler can do
// about a reply that will not send. What was missing is the record. Discord answers a rejected
// callback by showing the person who typed the command its own notice, "Missing Permissions", and
// that notice was the only evidence anywhere: nothing reached this process's log, so a channel
// whose overwrites deny the bot read as the bot being broken.
//
// The permission hint is the point. `50013` names no permission and no channel, and the bot's
// server-wide role can hold everything while one channel's overwrites deny it — which is not
// somewhere an operator thinks to look unless told.
pub(super) fn report_interaction_failure(
    what: &str,
    command: &serenity::all::CommandInteraction,
    error: &serenity::Error,
) {
    let guild = command.guild_id.map(|id| id.get().to_string()).unwrap_or_else(|| "DM".to_string());
    eprintln!(
        "[discord] {what} failed for /{} in channel {} (guild {}): {error}",
        command.data.name,
        command.channel_id.get(),
        guild,
    );
    for line in permission_hint(error, command) {
        eprintln!("[discord] {line}");
    }
}

// The same for a reply that is not answering a command — a reaction handler, a component.
pub(super) fn report_send_failure(what: &str, channel_id: u64, error: &serenity::Error) {
    eprintln!("[discord] {what} failed in channel {channel_id}: {error}");
    for line in generic_permission_hint(error, channel_id) {
        eprintln!("[discord] {line}");
    }
}

// Discord's code for "you may not do that here". `50001` is Missing Access, which is the same
// story told from one step further back: the bot cannot see the channel at all.
fn discord_error_code(error: &serenity::Error) -> Option<isize> {
    match error {
        serenity::Error::Http(serenity::http::HttpError::UnsuccessfulRequest(response)) => {
            Some(response.error.code)
        }
        _ => None,
    }
}

fn permission_hint(
    error: &serenity::Error,
    command: &serenity::all::CommandInteraction,
) -> Vec<String> {
    let mut lines = generic_permission_hint(error, command.channel_id.get());
    if !lines.is_empty() {
        // Which reply it was matters: an ephemeral one sends without `Send Messages`, so a command
        // that fails here while `/help` works in the same channel is that permission and not a rank.
        lines.push(format!(
            "a reply Discord refused is not an authorisation problem — /{} passed the rank gate to get this far",
            command.data.name,
        ));
    }
    lines
}

fn generic_permission_hint(error: &serenity::Error, channel_id: u64) -> Vec<String> {
    match discord_error_code(error) {
        Some(50013) | Some(50001) => vec![
            format!(
                "Discord refused this as a permission problem in channel {channel_id}. Check that channel's own overwrites, not just the bot's role: a role that holds everything server-wide is still denied by one channel."
            ),
            "Pandora needs View Channel, Send Messages, Embed Links, Add Reactions and Read Message History there — Send Messages in Threads too if the command is used in a thread. It posts a public message, reacts ❌ to it, and edits it into an embed as the job runs.".to_string(),
            "Ephemeral replies send without Send Messages, so an operator command answering normally in the same channel while this one fails is exactly that permission.".to_string(),
        ],
        _ => Vec::new(),
    }
}

// For a reply a call site builds itself. Every one of these used to end in `.await.ok()`, which is
// how `/help` — a command that bypasses the rank gate entirely and answers ephemerally — could be
// refused by Discord with nothing whatsoever reaching this log. That is the one command an operator
// is told to try when something looks broken, so it is the one that must say why it did not answer.
pub(super) fn or_report(
    result: Result<(), serenity::Error>,
    what: &str,
    command: &serenity::all::CommandInteraction,
) {
    if let Err(error) = result {
        report_interaction_failure(what, command, &error);
    }
}

pub(super) async fn command_error(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    content: impl Into<String>,
) {
    if let Err(error) = command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(content.into())
            .ephemeral(true)
    )).await {
        report_interaction_failure("error reply", command, &error);
    }
}

pub(super) async fn command_server_id(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    label: &str,
) -> Option<u64> {
    match command.guild_id {
        Some(g) => Some(g.get()),
        None => {
            command_error(ctx, command, format!("Error: {} can only be used in a server", label)).await;
            None
        }
    }
}

pub(super) async fn positive_u32_option(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    name: &str,
) -> Option<u32> {
    match option_i64(command, name) {
        Some(n) if n >= 1 && n <= u32::MAX as i64 => Some(n as u32),
        _ => {
            command_error(ctx, command, format!("Error: `{}` must be a positive integer.", name)).await;
            None
        }
    }
}

pub(super) async fn required_trimmed_option(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    name: &str,
    label: &str,
) -> Option<String> {
    match option_trimmed(command, name) {
        Some(s) => Some(s),
        None => {
            command_error(ctx, command, format!("Error: {} is required", label)).await;
            None
        }
    }
}

pub(super) async fn working_response(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    content: &str,
) -> Option<Message> {
    let trimmed = content.trim();
    let content = if trimmed.ends_with("...") || trimmed.ends_with('…') {
        command_message(command, COMMAND_WORKING)
    } else {
        content.to_string()
    };
    // The one reply in the whole bot that is deliberately public: it is the message the job then
    // lives in, edited into an embed and reacted to as it runs, so it cannot be ephemeral. That
    // also makes it the first thing a channel's permissions can refuse, and the failure used to be
    // swallowed twice — here and on the fetch below — leaving the command doing nothing at all
    // with not one line to say why.
    if let Err(error) = command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content(content)
    )).await {
        report_interaction_failure("job message", command, &error);
        return None;
    }
    match command.get_response(&ctx.http).await {
        Ok(message) => Some(message),
        Err(error) => {
            report_interaction_failure("job message read-back", command, &error);
            None
        }
    }
}

pub(super) fn command_language(command: &serenity::all::CommandInteraction) -> String {
    let Some(guild_id) = command.guild_id else {
        return "tr".to_string();
    };
    std::fs::read_to_string(format!("DB/config/{}/meta.pandora", guild_id.get()))
        .ok()
        .and_then(|content| content.lines().next().map(str::to_string))
        .filter(|lang| !lang.trim().is_empty())
        .unwrap_or_else(|| "tr".to_string())
}

pub(super) fn command_message(
    command: &serenity::all::CommandInteraction,
    id: &str,
) -> String {
    get_message(id, &command_language(command))
}

pub(super) fn command_format(
    command: &serenity::all::CommandInteraction,
    id: &str,
    args: &[String],
) -> String {
    format_message(id, &command_language(command), args)
}

pub(super) fn success_embed(
    command: &serenity::all::CommandInteraction,
    title_id: &str,
) -> CreateEmbed {
    command_embed(command, title_id, Colour::DARK_GREEN)
}

pub(super) fn info_embed(
    command: &serenity::all::CommandInteraction,
    title_id: &str,
) -> CreateEmbed {
    command_embed(command, title_id, Colour::BLUE)
}

fn command_embed(
    command: &serenity::all::CommandInteraction,
    title_id: &str,
    colour: Colour,
) -> CreateEmbed {
    let lang = command_language(command);
    CreateEmbed::new()
        .title(get_message(title_id, &lang))
        .colour(colour)
        .footer(CreateEmbedFooter::new(format_message(
            EMBED_FOOTER,
            &lang,
            &[PKGVER.to_string()],
        )))
        .timestamp(serenity::model::Timestamp::now())
}

pub(super) async fn edit_response_embed(
    ctx: &Context,
    response: &mut Message,
    embed: CreateEmbed,
) {
    let _ = response
        .edit(ctx, EditMessage::new().content("").embed(embed))
        .await;
}

pub(super) async fn attached_repo(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    server_id: u64,
    episode: Option<u32>,
) -> Option<(ChannelMeta, String, String)> {
    let meta = read_channel_meta(server_id, command.channel_id.get());
    if meta.mal_id.is_none() {
        command_error(ctx, command, "Error: this channel is not attached to an anime. Run `/init` or `/attach` first.").await;
        return None;
    }
    if let Some(episode) = episode {
        let max_ep = meta.episode_count.unwrap_or(0);
        if episode < 1 || episode > max_ep {
            command_error(ctx, command, format!("Error: `episode` must be between 1 and {}.", max_ep)).await;
            return None;
        }
    }
    let repo_url = match meta.repo_url.clone().filter(|s| !s.is_empty()) {
        Some(u) => u,
        None => {
            command_error(ctx, command, "Error: this channel has no repo URL configured.").await;
            return None;
        }
    };
    let (owner, repo) = match parse_repo_url(&repo_url) {
        Ok(t) => t,
        Err(e) => {
            command_error(ctx, command, format!("Error: bad repo URL in meta: {}", e)).await;
            return None;
        }
    };
    Some((meta, format!("{}/{}", owner, repo), repo_url))
}

pub(super) async fn forgejo_config(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    server_id: u64,
) -> Option<(String, String)> {
    let (_lang, forgejo_base, api_key) = match read_server_meta(server_id).await {
        Ok(t) => t,
        Err(e) => {
            command_error(ctx, command, format!("Error: failed to read server meta: {}", e)).await;
            return None;
        }
    };
    if forgejo_base.is_empty() {
        command_error(ctx, command, "Error: server has no forgejo org configured. Run `/configure` first.").await;
        return None;
    }
    Some((forgejo_base, api_key))
}
