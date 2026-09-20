use super::*;

use pandora_toolchain::lib::source_doc::{compose as compose_source, ProbeRef};
use pandora_toolchain::pnworker::messages::{
    COMMAND_SOURCE_PICK, FIELD_PROGRESS, PICK_PROMPT, PICK_TIMEOUT,
};
use pandora_toolchain::pnworker::probe_pages::{
    probe_page_body, probe_page_components, probe_page_count,
};
use tokio::sync::mpsc::Sender;

// How long a pack's file list waits for its index, the same window a queued job's list gets.
const SOURCE_PICK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

// `/source` takes a source link. A season pack has no single "the" episode in it, so a link on its
// own cannot say which file episode 3 is: a torrent is listed first, and when it holds more than
// one video the file is asked for in chat and written down beside the link, which `/smartcode do`
// reads back instead of asking again.
pub async fn handle_source(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    tx: &Sender<JobClass>,
) {
    let episode = match positive_u32_option(ctx, command, "episode").await {
        Some(n) => n,
        None => return,
    };
    let Some(link) = required_trimmed_option(ctx, command, "link", "Source link").await else {
        return;
    };
    let server_id = match command_server_id(ctx, command, "/source").await {
        Some(id) => id,
        None => return,
    };
    let (_meta, owner_repo, repo_url) = match attached_repo(ctx, command, server_id, Some(episode)).await {
        Some(t) => t,
        None => return,
    };
    let (forgejo_base, api_key) = match forgejo_config(ctx, command, server_id).await {
        Some(t) => t,
        None => return,
    };
    let mut response_msg = match working_response(ctx, command, "Working…").await {
        Some(m) => m,
        None => return,
    };

    let fg = match Forgejo::new(forgejo_base, api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Forgejo init failed: {}", e))).await;
            return;
        }
    };

    let Some(probe) = resolve_pack_file(ctx, command, tx, &mut response_msg, &link).await else {
        return;
    };

    let folder = pad2(episode);
    let source_path = format!("{}/SOURCE.md", folder);
    let source_content = compose_source(&source_link(&link), probe);
    let source_b64 = base64_encode(&source_content);
    match fg.upsert_file(&owner_repo, &source_path, &source_b64, "Set source link").await {
        Ok(()) => {
            remove_gitkeep_for_path(&fg, &owner_repo, &source_path).await;
            pandora_toolchain::lib::git::record_attachment_sync(server_id, command.channel_id.get()).await;
            let source_display = if link.starts_with("magnet:") {
                command_message(command, VALUE_MAGNET_HIDDEN)
            } else {
                source_link(&link)
            };
            let mut embed = success_embed(command, COMMAND_SOURCE_UPDATED)
                .field(
                    command_message(command, FIELD_REPO),
                    format!("[{}]({})", owner_repo, repo_url),
                    true,
                )
                .field(
                    command_message(command, FIELD_EPISODE),
                    format!("`{}`", episode),
                    true,
                )
                .field(
                    command_message(command, FIELD_PATH),
                    format!("`{}`", source_path),
                    false,
                )
                .field(
                    command_message(command, FIELD_SOURCE),
                    source_display,
                    false,
                );
            if let Some(probe) = probe {
                embed = embed.field(
                    command_message(command, FIELD_FILE),
                    format!("`#{}`", probe.file_index),
                    false,
                );
            }
            let _ = response_msg
                .edit(ctx, EditMessage::new().content("").embed(embed).components(vec![]))
                .await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to write `{}`: {}", source_path, e))).await;
        }
    }
}

// Which file of the link this episode is, when the link is a pack. The outer `None` means the
// command is over — the listing failed or nobody answered, and the message already says so; the
// inner one means there was nothing to choose, which is every Drive link, direct link and
// single-video torrent, and those write the one-line `SOURCE.md` they always have.
async fn resolve_pack_file(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    tx: &Sender<JobClass>,
    response_msg: &mut Message,
    link: &str,
) -> Option<Option<ProbeRef>> {
    if !is_listable_source(link) {
        return Some(None);
    }
    let listing = match list_source(tx, command, link).await {
        Ok(listing) => listing,
        Err(reason) => {
            let _ = response_msg
                .edit(ctx, EditMessage::new().content(format!("Error: {}", reason)))
                .await;
            return None;
        }
    };
    if listing.rows.len() < 2 {
        return Some(None);
    }

    let lang = read_lang(command.guild_id);
    let embed = info_embed(command, COMMAND_SOURCE_PICK)
        .description(get_message(PICK_PROMPT, &lang))
        .field(
            get_message(FIELD_PROGRESS, &lang),
            probe_page_body(&listing.text, 1, &lang),
            false,
        );
    // The page buttons are the probe's own: they re-read the list from its stored progress and
    // swap it into whichever embed they were clicked on, keeping the prompt above it.
    let _ = response_msg
        .edit(
            ctx,
            EditMessage::new().content("").embed(embed).components(probe_page_components(
                listing.probe_job_id,
                1,
                probe_page_count(&listing.text),
            )),
        )
        .await;

    let offered = listing.rows.iter().map(|(index, _)| *index).collect();
    let picked = await_pending_pick(
        command.user.id.get(),
        command.channel_id.get(),
        offered,
        SOURCE_PICK_TIMEOUT,
    )
    .await;
    match picked {
        Some(file_index) => Some(Some(ProbeRef { job_id: listing.probe_job_id, file_index })),
        None => {
            let _ = response_msg
                .edit(
                    ctx,
                    EditMessage::new()
                        .content(get_message(PICK_TIMEOUT, &lang))
                        .embeds(vec![])
                        .components(vec![]),
                )
                .await;
            None
        }
    }
}
