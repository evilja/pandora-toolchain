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

    let credit_overrides = match pandora_toolchain::pnworker::acix::CreditOverrides::from_values(
        option_str(command, "extra").map(str::to_string),
        option_str(command, "tl").map(str::to_string),
        option_str(command, "tlc").map(str::to_string),
        option_str(command, "ts").map(str::to_string),
        option_str(command, "qc").map(str::to_string),
    ) {
        Ok(overrides) => overrides,
        Err(e) => {
            command_error(ctx, command, format!("Error: {}", e)).await;
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

    match pandora_toolchain::pnworker::acix::confirm_acix_with_overrides(&db, job_id, credit_overrides).await {
        Ok(_) => {
            acixconfirm_response(
                ctx,
                command,
                format!("Published job `{}` to AnimeciX multishare and multiple.", job_id),
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
