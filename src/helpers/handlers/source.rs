use super::*;

use pandora_toolchain::lib::source_doc::{compose as compose_source, ProbeRef};

// `/source` takes either a source link or a `/probe` result plus a file index, the same pair
// `/encode pan` and `/subs` take. A season pack has no single "the" episode in it, so a link on its
// own cannot say which file episode 3 is — the probe form writes that down beside the link, and
// `/smartcode pan` reads it back.
pub async fn handle_source(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let episode = match positive_u32_option(ctx, command, "episode").await {
        Some(n) => n,
        None => return,
    };
    let Some((link, probe)) = resolve_source(ctx, command).await else {
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
                    format!("`#{}` of probe `{}`", probe.file_index, probe.job_id),
                    false,
                );
            }
            edit_response_embed(ctx, &mut response_msg, embed).await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to write `{}`: {}", source_path, e))).await;
        }
    }
}

// The link a `SOURCE.md` will carry, and the probe it was picked out of when there was one.
async fn resolve_source(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) -> Option<(String, Option<ProbeRef>)> {
    let link = option_trimmed(command, "link");
    let probe_job_id = option_str(command, "job_id").map(str::trim).filter(|id| !id.is_empty());

    if link.is_none() && probe_job_id.is_none() {
        command_error(ctx, command, "Error: pass either `link` or `job_id` with `index`.").await;
        return None;
    }
    if link.is_some() && probe_job_id.is_some() {
        command_error(ctx, command, "Error: pass `link` or `job_id`, not both.").await;
        return None;
    }

    let Some(raw) = probe_job_id else {
        return Some((link.unwrap_or_default(), None));
    };
    let Ok(job_id) = raw.parse::<u64>() else {
        command_error(ctx, command, "Error: job_id must be a number").await;
        return None;
    };
    let file_index = match option_i64(command, "index") {
        Some(index) if index >= 0 => index as u64,
        _ => {
            command_error(ctx, command, "Error: `index` is required with `job_id`.").await;
            return None;
        }
    };
    let db = match JobDb::new().await {
        Ok(db) => db,
        Err(e) => {
            command_error(ctx, command, format!("Error: failed to open job DB: {}", e)).await;
            return None;
        }
    };
    let link = match db.get_job(job_id).await {
        Ok(Some(row)) => row.link,
        Ok(None) => {
            command_error(ctx, command, "Error: probe job was not found.").await;
            return None;
        }
        Err(e) => {
            command_error(ctx, command, format!("Error: failed to read probe job: {}", e)).await;
            return None;
        }
    };
    Some((link, Some(ProbeRef { job_id, file_index })))
}
