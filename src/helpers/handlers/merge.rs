use super::*;

use pandora_toolchain::lib::channel_link::LINK_USE_MERGE;
use pandora_toolchain::pnworker::server_config::server_merge_release_only;
use serenity::builder::CreateAttachment;

// Discord refuses an attachment past this on a server with no boosts, and a release ASS is orders
// of magnitude smaller — but a typeset script with embedded fonts is not, and a merge that cannot
// answer with the file still has an embed to answer with.
const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;

pub async fn handle_merge(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let mut response_msg = match working_response(ctx, command, "Working…").await {
        Some(m) => m,
        None => return,
    };
    let result = match smartcode_merge_upload(ctx, command, &mut response_msg, "/merge", "merge", None).await {
        Some(r) => r,
        None => return,
    };

    // `/edit merge_release_only` — some groups want the file, not a description of where the file
    // went. The warnings still come with it: they are the only sign that a style went missing.
    let release_only = match command.guild_id {
        Some(guild_id) => server_merge_release_only(guild_id.get()).await,
        None => false,
    };
    // `/link set channel:#somewhere` — the file is wanted where the group reads, not where its
    // editors type. Only the file moves: the command still answers in the channel it was run in,
    // because whoever ran it is looking here.
    let redirect = match command.guild_id {
        Some(guild_id) if release_only => {
            link_target(guild_id.get(), command.channel_id.get(), LINK_USE_MERGE).await
        }
        _ => None,
    };
    let mut sent_to: Option<String> = None;
    if release_only && result.merged_bytes.len() <= MAX_ATTACHMENT_BYTES {
        let file_name = result
            .release_path
            .rsplit('/')
            .next()
            .unwrap_or("release.ass")
            .to_string();
        let mut notes = result
            .warnings
            .iter()
            .map(|warning| format!("-# {}", warning))
            .collect::<Vec<_>>();
        let mut delivered = false;
        if let Some(target) = redirect {
            let message = CreateMessage::new()
                .content(notes.join("\n"))
                .add_file(CreateAttachment::bytes(
                    result.merged_bytes.clone(),
                    file_name.clone(),
                ));
            match target.send_message(ctx, message).await {
                Ok(sent) => {
                    sent_to = Some(sent.link());
                    delivered = true;
                }
                Err(error) => {
                    // A linked channel Pandora cannot post in is worth saying out loud: the file
                    // is not missing, it is one permission away. It still comes back here, which
                    // is where it would have gone had nothing been linked — with the reason
                    // beside it, since the embed that would otherwise carry it is replaced.
                    report_send_failure("merge release redirect", target.get(), &error);
                    notes.insert(0, format!(
                        "-# <#{}> refused the release — Pandora needs View Channel, Send Messages and Attach Files there.",
                        target.get()
                    ));
                }
            }
        }
        if !delivered {
            let edit = EditMessage::new()
                .content(notes.join("\n"))
                .embeds(Vec::new())
                .new_attachment(CreateAttachment::bytes(result.merged_bytes.clone(), file_name));
            if response_msg.edit(ctx, edit).await.is_ok() {
                return;
            }
            // The file could not be sent; the embed below is the answer that is left.
        }
    }

    let mut embed = success_embed(command, COMMAND_MERGE_COMPLETE)
        .field(
            command_message(command, FIELD_REPO),
            format!("`{}`", result.owner_repo),
            true,
        )
        .field(
            command_message(command, FIELD_RELEASE),
            format!("`{}`", result.release_path),
            true,
        )
        .field(
            command_message(command, FIELD_SOURCE),
            format!("`{}`", result.source_path),
            false,
        )
        .field(
            command_message(command, FIELD_WARNINGS),
            format_warnings_field(&result.warnings, command),
            false,
        );
    if let Some(sent_to) = sent_to {
        embed = embed.field(command_message(command, FIELD_CHANNEL), sent_to, false);
    }
    edit_response_embed(ctx, &mut response_msg, embed).await;
}
