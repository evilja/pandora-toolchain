use super::*;
use pandora_toolchain::lib::db::core::{stage_from_int, JobDb};
use pandora_toolchain::pnworker::core::Stage;
use pandora_toolchain::pnworker::messages::{
    get_stage_text, JOB_TYPE_BACKUP, JOB_TYPE_BACKUP_ALL, JOB_TYPE_BATCH, JOB_TYPE_DISTRIBUTE,
    JOB_TYPE_ENCODE, JOB_TYPE_KEYCODE, JOB_TYPE_PANCODE, JOB_TYPE_PREVIEW, JOB_TYPE_PROBE,
    JOB_TYPE_STUDIO, JOB_TYPE_STUDIO_PREVIEW, JOB_TYPE_SUBS, QUEUE_EMPTY, QUEUE_LINE, QUEUE_MORE,
    QUEUE_TITLE,
};

// Discord caps a message at 2000 characters and every row carries an id, a type and a stage, so
// this is what fits with the heading and the "and N more" footer still on screen.
const QUEUE_ROWS_SHOWN: usize = 15;

// The label for a stored job-type number, reusing the same catalogue the API and `/job` show. A
// number this binary does not know is shown as `job 42` rather than dropped: a queue display that
// silently hides a row is worse than one with an unfamiliar name in it.
fn job_type_key(job_type: i64) -> Option<&'static str> {
    Some(match job_type {
        1 => JOB_TYPE_ENCODE,
        6 => JOB_TYPE_PANCODE,
        5 => JOB_TYPE_PROBE,
        8 => JOB_TYPE_BACKUP,
        9 => JOB_TYPE_BACKUP_ALL,
        10 => JOB_TYPE_KEYCODE,
        13 => JOB_TYPE_PREVIEW,
        14 => JOB_TYPE_STUDIO,
        15 => JOB_TYPE_STUDIO_PREVIEW,
        16 => JOB_TYPE_BATCH,
        17 => JOB_TYPE_SUBS,
        21 => JOB_TYPE_DISTRIBUTE,
        _ => return None,
    })
}

// What is waiting and what is running, from the table the worker already writes. Reading the row
// rather than the loop's `Vec<Job>` is what makes this work from a handler at all, and it is the
// same source `/job` and the web console read, so the three never disagree.
pub async fn handle_queue(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let db = match JobDb::shared().await {
        Ok(db) => db,
        Err(e) => {
            command_error(ctx, command, format!("Error: the job database is unavailable: {e}")).await;
            return;
        }
    };
    let rows = match db.get_ongoing_jobs().await {
        Ok(rows) => rows,
        Err(e) => {
            command_error(ctx, command, format!("Error: the queue could not be read: {e}")).await;
            return;
        }
    };
    let requester = command.user.id.get();
    // Everyone sees the work they asked for. From the worker rank up the whole machine's queue is
    // visible, because "why is my job not moving" is a question only an operator can answer.
    let everywhere = has_level_at_least(requester, 2);
    let visible: Vec<_> = rows
        .iter()
        .filter(|row| everywhere || row.author.max(0) as u64 == requester)
        .collect();

    if visible.is_empty() {
        let text = command_format(command, QUEUE_EMPTY, &[]);
        let _ = command
            .create_response(
                ctx,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new().content(text).ephemeral(true),
                ),
            )
            .await;
        return;
    }

    // Waiting is `Queued` and nothing else: every other non-terminal stage has a worker holding
    // the job. Both counts describe the rows actually shown, not the whole table, so the heading
    // never claims more than the list below it.
    let waiting = visible
        .iter()
        .filter(|row| stage_from_int(row.stage) == Some(Stage::Queued))
        .count();
    let running = visible.len() - waiting;

    let mut lines = vec![command_format(
        command,
        QUEUE_TITLE,
        &[running.to_string(), waiting.to_string()],
    )];
    for row in visible.iter().take(QUEUE_ROWS_SHOWN) {
        let job_type = match job_type_key(row.job_type) {
            Some(key) => command_format(command, key, &[]),
            None => format!("job {}", row.job_type),
        };
        // `stage_label` is the internal English name; what the reader needs is the localized
        // wording `/job` already shows for the same stage. A number this build does not know is
        // printed as-is rather than dropped, for the same reason an unknown job type is.
        let stage = match stage_from_int(row.stage) {
            Some(stage) => get_stage_text(stage, &command_language(command)),
            None => format!("stage {}", row.stage),
        };
        lines.push(command_format(
            command,
            QUEUE_LINE,
            &[row.job_id.to_string(), job_type, stage],
        ));
    }
    if visible.len() > QUEUE_ROWS_SHOWN {
        lines.push(command_format(
            command,
            QUEUE_MORE,
            &[(visible.len() - QUEUE_ROWS_SHOWN).to_string()],
        ));
    }

    let _ = command
        .create_response(
            ctx,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(lines.join("\n"))
                    .ephemeral(true),
            ),
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    // A queue row whose type has no label would show a bare number to a user; every type the
    // worker can leave in a non-terminal row is expected to be named.
    #[test]
    fn every_queued_job_type_has_a_label() {
        for known in [1, 5, 6, 8, 9, 10, 13, 14, 15, 16, 17, 21] {
            assert!(job_type_key(known).is_some(), "{known}");
        }
        assert_eq!(job_type_key(999), None);
    }
}
