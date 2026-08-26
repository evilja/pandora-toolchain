use super::*;
use pandora_toolchain::lib::publishlog::log_publish;

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
    log_publish(job_id, "/acixconfirm", format!("invoked by user {} in channel {}", command.user.id, command.channel_id)).await;

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
            acixconfirm_response(ctx, command, job_id, format!("Database error: {}", e)).await;
            return;
        }
    };

    match pandora_toolchain::pnworker::acix::confirm_acix_with_overrides(&db, job_id, credit_overrides).await {
        Ok(_) => {
            acixconfirm_response(
                ctx,
                command,
                job_id,
                format!("Published job `{}` to AnimeciX multishare and multiple.", job_id),
            )
            .await;
        }
        Err(e) => {
            acixconfirm_response(
                ctx,
                command,
                job_id,
                format!("AnimeciX publish failed: {}", e),
            )
            .await;
        }
    }
}

async fn acixconfirm_response(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    job_id: u64,
    content: impl Into<String>,
) {
    let content = content.into();
    log_publish(job_id, "/acixconfirm", &content).await;
    // The command defers before it talks to the provider, so a lost reply edit leaves the
    // ephemeral stuck on "thinking" with no trace anywhere else. Record it either way.
    if let Err(e) = command
        .edit_response(ctx, EditInteractionResponse::new().content(content))
        .await
    {
        eprintln!("[/acixconfirm] response edit failed: {}", e);
        log_publish(job_id, "/acixconfirm", format!("response edit failed: {}", e)).await;
    }
}
