use super::*;
use pandora_toolchain::lib::joblog::{JobLogLocation, find_job_logs, zip_log_files};
use serenity::builder::CreateAttachment;

const MAX_LOG_ARCHIVE_BYTES: usize = 24 * 1024 * 1024;

struct LogArchive {
    bytes: Vec<u8>,
    files: usize,
    location_id: &'static str,
}

pub async fn handle_catlogs(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let job_id = match option_str(command, "job_id").and_then(|value| value.parse::<u64>().ok()) {
        Some(job_id) => job_id,
        None => {
            command_error(ctx, command, "Error: `job_id` must be a number.").await;
            return;
        }
    };

    command
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await
        .ok();

    let archive = match find_log_archive(job_id).await {
        Ok(Some(archive)) => archive,
        Ok(None) => {
            let text = command_format(command, CATLOGS_NO_LOGS, &[job_id.to_string()]);
            command
                .edit_response(ctx, EditInteractionResponse::new().content(text))
                .await
                .ok();
            return;
        }
        Err(error) => {
            let text = command_format(
                command,
                CATLOGS_BUILD_FAIL,
                &[job_id.to_string(), error],
            );
            command
                .edit_response(ctx, EditInteractionResponse::new().content(text))
                .await
                .ok();
            return;
        }
    };

    if archive.bytes.len() > MAX_LOG_ARCHIVE_BYTES {
        let text = command_format(
            command,
            CATLOGS_BUILD_FAIL,
            &[
                job_id.to_string(),
                format!(
                    "archive is {:.1} MiB (the Discord-safe limit is 24 MiB)",
                    archive.bytes.len() as f64 / 1024.0 / 1024.0
                ),
            ],
        );
        command
            .edit_response(ctx, EditInteractionResponse::new().content(text))
            .await
            .ok();
        return;
    }

    let location = command_message(command, archive.location_id);
    let description = command_format(
        command,
        CATLOGS_DESCRIPTION,
        &[archive.files.to_string(), location.clone()],
    );
    let embed = success_embed(command, COMMAND_LOGS_READY)
        .description(description)
        .field(
            command_message(command, FIELD_JOBID),
            format!("`{}`", job_id),
            true,
        )
        .field(
            command_message(command, FIELD_FILES),
            format!("`{}`", archive.files),
            true,
        )
        .field(
            command_message(command, FIELD_LOCATION),
            location,
            true,
        );
    let attachment = CreateAttachment::bytes(
        archive.bytes,
        format!("pandora-logs-{}.zip", job_id),
    );
    command
        .edit_response(
            ctx,
            EditInteractionResponse::new()
                .content("")
                .embed(embed)
                .new_attachment(attachment),
        )
        .await
        .ok();
}

// The lookup and the zip live in `lib::joblog` so `/catlogs` and the API's
// `/jobs/:id/logs*` routes cannot drift apart; this only adds the Discord
// wording for where the logs were found.
async fn find_log_archive(job_id: u64) -> Result<Option<LogArchive>, String> {
    let logs = match find_job_logs(job_id).await? {
        Some(logs) => logs,
        None => return Ok(None),
    };
    let bytes = zip_log_files(&logs.files).await?;
    Ok(Some(LogArchive {
        bytes,
        files: logs.files.len(),
        location_id: match logs.location {
            JobLogLocation::Active => CATLOGS_ACTIVE,
            JobLogLocation::Archived => CATLOGS_ARCHIVED,
        },
    }))
}
