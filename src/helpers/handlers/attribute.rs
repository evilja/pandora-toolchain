use super::*;

use pandora_toolchain::lib::attribute::{
    add_once, dialogues_path, history_path, normalize_dialogue, read_list, read_styles,
    style_names, styles_path, write_list,
};
use serenity::builder::CreateAutocompleteResponse;

// Discord caps an autocomplete choice's value at 100 characters and a credit line is routinely
// longer, so a stored line is offered under a reference instead of under itself. Anything else the
// person types is taken as a dialogue line as written.
const REFERENCE_PREFIX: &str = "#";
const MAX_CHOICES: usize = 25;
const MAX_CHOICE_LABEL: usize = 100;

pub async fn handle_attribute(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let subcommand = subcommand_options(command)
        .map(|(name, _)| name)
        .unwrap_or("list");
    let Some(server_id) = command_server_id(ctx, command, "/attribute").await else {
        return;
    };
    // Attributes belong to the anime the channel is attached to, so an unattached channel has
    // nowhere to put them and nothing to spend them on.
    if attached_repo(ctx, command, server_id, None).await.is_none() {
        return;
    }
    let channel_id = command.channel_id.get();

    match subcommand {
        "set" => set_attribute(ctx, command, server_id, channel_id).await,
        "list" => list_attributes(ctx, command, server_id, channel_id).await,
        "remove" => remove_attribute(ctx, command, server_id, channel_id).await,
        "clear" => clear_attributes(ctx, command, server_id, channel_id).await,
        other => {
            command_error(ctx, command, format!("Unknown attribute subcommand `{}`.", other)).await;
        }
    }
}

async fn set_attribute(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    server_id: u64,
    channel_id: u64,
) {
    let file = option_attachment(command, "file");
    let typed = option_trimmed(command, "dialogue");
    if file.is_none() && typed.is_none() {
        command_error(ctx, command, "Error: give a `file`, a `dialogue`, or both.").await;
        return;
    }

    let mut notes: Vec<String> = Vec::new();
    if let Some(attachment) = file {
        let bytes = match attachment.download().await {
            Ok(bytes) => bytes,
            Err(e) => {
                command_error(ctx, command, format!("Failed to download `{}`: {}", attachment.filename, e)).await;
                return;
            }
        };
        let text = String::from_utf8_lossy(&bytes).to_string();
        // Refused here rather than at merge time: an attribute file that cannot supply styles is a
        // typo in an upload, and finding that out three commands later is finding it out too late.
        let styles = style_names(&text);
        if styles.is_empty() {
            command_error(
                ctx,
                command,
                format!("Error: `{}` defines no styles in a [V4+ Styles] section.", attachment.filename),
            )
            .await;
            return;
        }
        let path = styles_path(server_id, channel_id);
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                command_error(ctx, command, format!("Failed to create config dir: {}", e)).await;
                return;
            }
        }
        if let Err(e) = tokio::fs::write(&path, &bytes).await {
            command_error(ctx, command, format!("Failed to write attribute file: {}", e)).await;
            return;
        }
        notes.push(format!(
            "Styles now come from `{}` — `{}`.",
            attachment.filename,
            one_line(&styles.join("`, `"))
        ));
    }

    if let Some(typed) = typed {
        let mut history = read_list(history_path(server_id)).await;
        let line = match resolve_reference(&typed, &history) {
            Some(line) => line,
            None => match normalize_dialogue(&typed) {
                Ok(line) => line,
                Err(reason) => {
                    command_error(ctx, command, format!("Error: {}.", reason)).await;
                    return;
                }
            },
        };
        let mut dialogues = read_list(dialogues_path(server_id, channel_id)).await;
        if add_once(&mut dialogues, &line) {
            if let Err(e) = write_list(dialogues_path(server_id, channel_id), &dialogues).await {
                command_error(ctx, command, format!("Failed to write dialogues: {}", e)).await;
                return;
            }
            notes.push(format!("Dialogue `{}` added ({} in this channel).", one_line(&line), dialogues.len()));
        } else {
            notes.push(format!("Dialogue `{}` was already on this channel.", one_line(&line)));
        }
        if add_once(&mut history, &line) {
            if let Err(e) = write_list(history_path(server_id), &history).await {
                eprintln!("[attribute] could not remember dialogue for server {}: {}", server_id, e);
            }
        }
    }

    respond(ctx, command, COMMAND_UPDATED, notes.join("\n")).await;
}

async fn list_attributes(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    server_id: u64,
    channel_id: u64,
) {
    let styles = read_styles(server_id, channel_id).await;
    let dialogues = read_list(dialogues_path(server_id, channel_id)).await;
    let mut description = match &styles {
        Some(text) => format!(
            "**Styles**: `attribute.ass`, {} style(s): `{}`.",
            style_names(text).len(),
            one_line(&style_names(text).join("`, `"))
        ),
        None => "**Styles**: none; merges keep the styles the subtitles came with.".to_string(),
    };
    description.push_str("\n\n**Dialogues**");
    if dialogues.is_empty() {
        description.push_str("\nnone — nothing is injected into this channel's releases.");
    } else {
        for (index, line) in dialogues.iter().enumerate() {
            description.push_str(&format!("\n`{}{}` `{}`", REFERENCE_PREFIX, index + 1, one_line(line)));
        }
    }
    respond(ctx, command, COMMAND_LIST, description).await;
}

async fn remove_attribute(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    server_id: u64,
    channel_id: u64,
) {
    let Some(typed) = option_trimmed(command, "dialogue") else {
        command_error(ctx, command, "Error: `dialogue` is required.").await;
        return;
    };
    let mut dialogues = read_list(dialogues_path(server_id, channel_id)).await;
    let wanted = resolve_reference(&typed, &dialogues)
        .or_else(|| normalize_dialogue(&typed).ok())
        .unwrap_or(typed);
    let Some(index) = dialogues.iter().position(|line| *line == wanted) else {
        command_error(ctx, command, "Error: this channel has no such dialogue. Run `/attribute list` to see them.").await;
        return;
    };
    let removed = dialogues.remove(index);
    if let Err(e) = write_list(dialogues_path(server_id, channel_id), &dialogues).await {
        command_error(ctx, command, format!("Failed to write dialogues: {}", e)).await;
        return;
    }
    respond(
        ctx,
        command,
        COMMAND_UPDATED,
        format!("Removed `{}`. {} left on this channel.", one_line(&removed), dialogues.len()),
    )
    .await;
}

async fn clear_attributes(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    server_id: u64,
    channel_id: u64,
) {
    let what = option_str(command, "what").unwrap_or("all");
    let mut notes: Vec<String> = Vec::new();
    if matches!(what, "all" | "styles") {
        match tokio::fs::remove_file(styles_path(server_id, channel_id)).await {
            Ok(()) => notes.push("Styles file removed.".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                notes.push("There was no styles file.".to_string())
            }
            Err(e) => {
                command_error(ctx, command, format!("Failed to remove attribute file: {}", e)).await;
                return;
            }
        }
    }
    if matches!(what, "all" | "dialogues") {
        let count = read_list(dialogues_path(server_id, channel_id)).await.len();
        // Written empty rather than deleted: the history the autocomplete reads is a different
        // file, so clearing a channel never costs the server the lines it has written.
        if let Err(e) = write_list(dialogues_path(server_id, channel_id), &[]).await {
            command_error(ctx, command, format!("Failed to write dialogues: {}", e)).await;
            return;
        }
        notes.push(format!("{} dialogue(s) removed.", count));
    }
    respond(ctx, command, COMMAND_UPDATED, notes.join("\n")).await;
}

pub async fn handle_attribute_autocomplete(
    ctx: &Context,
    interaction: &serenity::all::CommandInteraction,
) {
    let mut response = CreateAutocompleteResponse::new();
    let Some(focused) = interaction.data.autocomplete() else {
        eprintln!("[attribute] autocomplete arrived with no focused option; answering with nothing");
        interaction
            .create_response(ctx, CreateInteractionResponse::Autocomplete(response))
            .await
            .ok();
        return;
    };
    if focused.name == "dialogue" {
        let subcommand = subcommand_options(interaction)
            .map(|(name, _)| name)
            .unwrap_or("set");
        let server_id = interaction.guild_id.map(|id| id.get());
        let channel_id = interaction.channel_id.get();
        // `remove` can only take away what this channel has; `set` also offers everything the
        // server has ever written, which is what makes a credit block reusable across anime.
        let lines = match (subcommand, server_id) {
            (_, None) => Vec::new(),
            ("remove", Some(server_id)) => read_list(dialogues_path(server_id, channel_id)).await,
            (_, Some(server_id)) => read_list(history_path(server_id)).await,
        };
        let partial = focused.value.to_lowercase();
        for (index, line) in lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.to_lowercase().contains(&partial))
            .take(MAX_CHOICES)
        {
            response = response.add_string_choice(
                truncate_label(line),
                format!("{}{}", REFERENCE_PREFIX, index + 1),
            );
        }
    }
    interaction
        .create_response(ctx, CreateInteractionResponse::Autocomplete(response))
        .await
        .ok();
}

// `#3` means the third line of whichever list the option was offered from. Anything else is the
// person's own text, including a line that merely starts with a `#`, since no ASS event does.
fn resolve_reference(typed: &str, list: &[String]) -> Option<String> {
    let index = typed.trim().strip_prefix(REFERENCE_PREFIX)?.parse::<usize>().ok()?;
    list.get(index.checked_sub(1)?).cloned()
}

// Backticked in an embed, so a backtick in the line would close the span early, and a newline
// would break the list it sits in.
fn one_line(text: &str) -> String {
    text.replace('`', "'").replace(['\n', '\r'], " ")
}


fn truncate_label(line: &str) -> String {
    let label = one_line(line);
    if label.chars().count() <= MAX_CHOICE_LABEL {
        return label;
    }
    label.chars().take(MAX_CHOICE_LABEL - 1).collect::<String>() + "…"
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
