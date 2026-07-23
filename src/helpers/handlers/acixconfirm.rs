use super::*;

pub async fn handle_acixconfirm(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let job_id = match option_str(command, "job_id").and_then(|s| s.trim().parse::<u64>().ok()) {
        Some(id) => id,
        None => {
            command_error(ctx, command, "Error: `job_id` must be a numeric job id.").await;
            return;
        }
    };

    if command
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await
        .is_err()
    {
        return;
    }

    let db = match pandora_toolchain::lib::db::core::JobDb::new().await {
        Ok(d) => d,
        Err(e) => {
            acixconfirm_response(ctx, command, format!("Database error: {}", e)).await;
            return;
        }
    };

    match pandora_toolchain::pnworker::acix::confirm_acix(&db, job_id).await {
        Ok(_) => {
            acixconfirm_response(
                ctx,
                command,
                format!("Published job `{}` to AnimeciX.", job_id),
            )
            .await;
        }
        Err(e) => {
            acixconfirm_response(
                ctx,
                command,
                format!("AnimeciX publish failed: {}", e),
            )
            .await;
        }
    }
}

async fn acixconfirm_response(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    content: impl Into<String>,
) {
    command
        .edit_response(ctx, EditInteractionResponse::new().content(content.into()))
        .await
        .ok();
}
