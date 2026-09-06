use super::*;

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
    if release_only && result.merged_bytes.len() <= MAX_ATTACHMENT_BYTES {
        let file_name = result
            .release_path
            .rsplit('/')
            .next()
            .unwrap_or("release.ass")
            .to_string();
        let attachment = CreateAttachment::bytes(result.merged_bytes.clone(), file_name);
        let content = if result.warnings.is_empty() {
            String::new()
        } else {
            result
                .warnings
                .iter()
                .map(|warning| format!("-# {}", warning))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let edit = EditMessage::new()
            .content(content)
            .embeds(Vec::new())
            .new_attachment(attachment);
        if response_msg.edit(ctx, edit).await.is_ok() {
            return;
        }
        // The file could not be sent; the embed below is the answer that is left.
    }

    let embed = success_embed(command, COMMAND_MERGE_COMPLETE)
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
    edit_response_embed(ctx, &mut response_msg, embed).await;
}
