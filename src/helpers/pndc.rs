use pandora_toolchain::pnworker::core::Preset;
use pandora_toolchain::pnworker::util::IntrosConfig;
use serenity::{
    all::{CommandDataOption, CommandDataOptionValue, Context, Message},
    builder::{CreateInteractionResponse, CreateInteractionResponseMessage},
};

use super::{parse_repo_url, read_channel_meta, read_server_meta, ChannelMeta};

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

pub(super) fn resolve_preset(command: &serenity::all::CommandInteraction, intros: &IntrosConfig) -> Preset {
    let candidates = option_str(command, "concat")
        .and_then(|group| intros.resolve(group));
    match option_str(command, "preset").unwrap_or("standard") {
        "gpu" | "standard" => Preset::Standard(candidates),
        "dummy" => Preset::Dummy(candidates),
        _ => Preset::PseudoLossless(candidates),
    }
}

pub(super) async fn command_error(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    content: impl Into<String>,
) {
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(content.into())
            .ephemeral(true)
    )).await.ok();
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
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content(content)
    )).await.ok();
    command.get_response(&ctx.http).await.ok()
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
