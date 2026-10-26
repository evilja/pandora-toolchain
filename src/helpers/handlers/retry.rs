use super::*;
use pandora_toolchain::lib::db::core::{stage_from_int, JobDb};
use pandora_toolchain::lib::p2p::nyaaise::nyaaise;
use pandora_toolchain::pnworker::core::{JobType, Stage};
use pandora_toolchain::pnworker::messages::{
    RETRY_NOT_YOURS, RETRY_QUEUED, RETRY_RUNNING, RETRY_TYPE, RETRY_UNKNOWN,
};
use pandora_toolchain::pnworker::server_effects::preset_from_name;

// The job types a retry can rebuild from a row. Each of them is "one link, one output": the source
// is the only input, so the row is a complete description of the work. A probe is a step inside
// another job, a batch owns per-episode children, and Studio/Subs carry uploaded state that the
// row never stored — re-running one of those from a link would invent a different job.
fn retryable(job_type: i64) -> Option<JobType> {
    Some(match job_type {
        1 => JobType::Encode,
        8 => JobType::Backup,
        21 => JobType::Distribute,
        _ => return None,
    })
}

// Re-queue a finished job from the row it left behind.
//
// Nothing here reconstructs the old `Job` — the row never held the preset snapshot, the watermark
// bytes or the keep request — so this is deliberately "ask for the same source again", using the
// server's settings as they are now. That is also what a person retyping the command would get,
// and it avoids a retry quietly using a preset the operator has since replaced.
pub async fn handle_retry(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    tx: &tokio::sync::mpsc::Sender<JobClass>,
) {
    let Some(job_id) = resolve_job_option(ctx, command, false).await else {
        return;
    };
    let db = match JobDb::shared().await {
        Ok(db) => db,
        Err(e) => {
            command_error(ctx, command, format!("Error: the job database is unavailable: {e}")).await;
            return;
        }
    };
    let row = match db.get_job(job_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let text = command_format(command, RETRY_UNKNOWN, &[job_id.to_string()]);
            command_error(ctx, command, text).await;
            return;
        }
        Err(e) => {
            command_error(ctx, command, format!("Error: job `{job_id}` could not be read: {e}")).await;
            return;
        }
    };

    // Only a job that has stopped can be retried: a second queue entry for work that is already
    // running would race the first for the same output directory.
    let finished = matches!(
        stage_from_int(row.stage),
        Some(Stage::Failed) | Some(Stage::Cancelled) | Some(Stage::Declined)
    );
    if !finished {
        let text = command_format(command, RETRY_RUNNING, &[job_id.to_string()]);
        command_error(ctx, command, text).await;
        return;
    }

    // Whoever asked for the job may retry it; from the worker rank up, anyone may retry anyone's,
    // which is what an operator needs when they are clearing a machine's failures.
    if row.author.max(0) as u64 != command.user.id.get() && !has_level_at_least(command.user.id.get(), 2) {
        let text = command_format(command, RETRY_NOT_YOURS, &[]);
        command_error(ctx, command, text).await;
        return;
    }

    let Some(job_type) = retryable(row.job_type) else {
        let text = command_format(command, RETRY_TYPE, &[job_id.to_string()]);
        command_error(ctx, command, text).await;
        return;
    };
    if row.link.trim().is_empty() {
        let text = command_format(command, RETRY_TYPE, &[job_id.to_string()]);
        command_error(ctx, command, text).await;
        return;
    }

    let Some(response_msg) = working_response(ctx, command, "...").await else {
        return;
    };
    let new_id = response_msg.id.get();
    let mut job = Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        new_id,
        job_type,
        new_id,
        nyaaise(&row.link),
        Vec::new(),
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|guild| guild.get()),
    );
    // The row names the preset it actually ran with. Falling back to the server's current one is
    // the honest answer when the name is gone, and `preset_from_name` returning `None` means the
    // operator deleted that preset file since — either way the job is better off than not running.
    if let Some(name) = row.preset_name.as_deref() {
        if let Some(preset) = preset_from_name(name, job.preset.concat().clone()) {
            job.preset = preset;
        }
    }
    // A pack has no single file to work on, so the reopened job asks which one — the same question
    // the original command asked.
    job.pick_file_first();

    let text = command_format(command, RETRY_QUEUED, &[job_id.to_string(), new_id.to_string()]);
    job.frontend.set_text(&text).await;
    tx.send(JobClass::Job(job)).await.ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    // A retry is only honest for a job the row fully describes: one link in, one output out. A
    // probe is a step inside another job, a batch owns children, and Studio/Subs carry uploaded
    // state the row never stored.
    #[test]
    fn only_link_owned_jobs_are_retryable() {
        assert_eq!(retryable(1), Some(JobType::Encode));
        assert_eq!(retryable(8), Some(JobType::Backup));
        assert_eq!(retryable(21), Some(JobType::Distribute));
        for other in [2, 5, 6, 9, 10, 13, 14, 15, 16, 17, 20] {
            assert_eq!(retryable(other), None, "{other}");
        }
    }
}
