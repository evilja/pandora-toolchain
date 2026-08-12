use super::*;

// `/subs` takes either a source link or a `/probe` result plus a file index. The
// probe path exists because a season pack has no single "the" video to extract
// from, and the direct path exists because a single-episode link should not need
// a probe run first.
pub async fn handle_subs(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) -> Option<Job> {
    let torrent_url = option_trimmed(command, "torrent");
    let probe_job_id = option_str(command, "job_id").map(str::trim).filter(|id| !id.is_empty());
    let file_index = option_i64(command, "index");

    if torrent_url.is_none() && probe_job_id.is_none() {
        command_error(
            ctx,
            command,
            "Error: pass either `torrent` or `job_id` with `index`.",
        )
        .await;
        return None;
    }
    if torrent_url.is_some() && probe_job_id.is_some() {
        command_error(
            ctx,
            command,
            "Error: pass `torrent` or `job_id`, not both.",
        )
        .await;
        return None;
    }

    let (source, probe_job_id, file_index) = match probe_job_id {
        Some(raw) => {
            let Ok(probe_job_id) = raw.parse::<u64>() else {
                command_error(ctx, command, "Error: job_id must be a number").await;
                return None;
            };
            let index = match file_index {
                Some(index) if index >= 0 => index as u64,
                _ => {
                    command_error(ctx, command, "Error: `index` is required with `job_id`.").await;
                    return None;
                }
            };
            let db = match JobDb::new().await {
                Ok(db) => db,
                Err(e) => {
                    command_error(ctx, command, format!("Error: failed to open job DB: {}", e))
                        .await;
                    return None;
                }
            };
            let source = match db.get_job(probe_job_id).await {
                Ok(Some(row)) => row.link,
                Ok(None) => {
                    command_error(ctx, command, "Error: probe job was not found.").await;
                    return None;
                }
                Err(e) => {
                    command_error(ctx, command, format!("Error: failed to read probe job: {}", e))
                        .await;
                    return None;
                }
            };
            (source, Some(probe_job_id), Some(index))
        }
        None => (torrent_url.unwrap_or_default(), None, None),
    };

    let response_msg = working_response(ctx, command, "...").await?;
    response_msg.react(ctx, '❌').await.ok();

    let mut job = Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Subs,
        response_msg.id.get(),
        nyaaise(&source),
        Vec::new(),
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|guild| guild.get()),
    );
    job.display_link = Some(match file_index {
        Some(index) => format!("{} • file #{}", display_source_link(&source), index),
        None => display_source_link(&source),
    });
    job.probe_job_id = probe_job_id;
    job.probe_file_index = file_index;
    Some(job)
}
