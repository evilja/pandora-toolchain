use super::*;
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

async fn find_log_archive(job_id: u64) -> Result<Option<LogArchive>, String> {
    let mut last_error = None;
    for (directory, location_id) in [
        (
            PathBuf::from("DB").join("work").join(job_id.to_string()).join("log"),
            CATLOGS_ACTIVE,
        ),
        (
            PathBuf::from("DB")
                .join("saved_data")
                .join(job_id.to_string())
                .join("log"),
            CATLOGS_ARCHIVED,
        ),
    ] {
        let files = match log_files(&directory).await {
            Ok(files) => files,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        if files.is_empty() {
            continue;
        }
        let bytes = match zip_log_files(&files).await {
            Ok(bytes) => bytes,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        return Ok(Some(LogArchive {
            bytes,
            files: files.len(),
            location_id,
        }));
    }
    match last_error {
        Some(error) => Err(error),
        None => Ok(None),
    }
}

async fn log_files(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut files = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|error| error.to_string())? {
        let kind = entry.file_type().await.map_err(|error| error.to_string())?;
        if kind.is_file() {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

async fn zip_log_files(files: &[PathBuf]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut writer = async_zip::base::write::ZipFileWriter::new(&mut out);
        for path in files {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| format!("invalid log filename: {}", path.display()))?;
            let bytes = tokio::fs::read(path)
                .await
                .map_err(|error| format!("{}: {}", path.display(), error))?;
            let entry = async_zip::ZipEntryBuilder::new(
                name.to_string().into(),
                async_zip::Compression::Deflate,
            );
            writer
                .write_entry_whole(entry, &bytes)
                .await
                .map_err(|error| error.to_string())?;
        }
        writer.close().await.map_err(|error| error.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn log_archive_collects_only_files_and_writes_zip() {
        let root = std::env::temp_dir().join(format!(
            "pandora-catlogs-test-{}",
            std::process::id()
        ));
        tokio::fs::remove_dir_all(&root).await.ok();
        tokio::fs::create_dir_all(root.join("nested")).await.unwrap();
        tokio::fs::write(root.join("encode.log"), b"encode log")
            .await
            .unwrap();
        tokio::fs::write(root.join("upload.log"), b"upload log")
            .await
            .unwrap();

        let files = log_files(&root).await.unwrap();
        assert_eq!(files.len(), 2);
        let archive = zip_log_files(&files).await.unwrap();
        assert!(archive.starts_with(b"PK"));

        tokio::fs::remove_dir_all(root).await.ok();
    }
}
