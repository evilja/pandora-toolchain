use super::*;
use pandora_toolchain::pnworker::acix::{unpublish_acix, AcixResetScope};

pub async fn handle_acixunpublish(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let job_id = match option_str(command, "job_id").and_then(|value| value.trim().parse::<u64>().ok()) {
        Some(job_id) => job_id,
        None => {
            command_error(ctx, command, "Error: `job_id` must be a numeric job id.").await;
            return;
        }
    };
    let scope = match option_str(command, "scope").and_then(AcixResetScope::parse) {
        Some(scope) => scope,
        None => {
            command_error(ctx, command, "Error: `scope` must be multiple, multishare, or both.").await;
            return;
        }
    };

    if command.create_response(
        ctx,
        CreateInteractionResponse::Defer(
            CreateInteractionResponseMessage::new().ephemeral(true),
        ),
    ).await.is_err() {
        return;
    }

    let db = match pandora_toolchain::lib::db::core::JobDb::new().await {
        Ok(db) => db,
        Err(e) => {
            command.edit_response(
                ctx,
                EditInteractionResponse::new().content(format!("Database error: {}", e)),
            ).await.ok();
            return;
        }
    };

    match unpublish_acix(&db, job_id, scope).await {
        Ok(result) => {
            let status = result.get("status").and_then(|value| value.as_str()).unwrap_or("pending");
            let scope = result.get("scope").and_then(|value| value.as_str()).unwrap_or("selected");
            command.edit_response(
                ctx,
                EditInteractionResponse::new().content(format!(
                    "Reset local AnimeciX `{}` state for job `{}`. New local status: `{}`. No AnimeciX videos were remotely deleted.",
                    scope, job_id, status,
                )),
            ).await.ok();
        }
        Err(e) => {
            command.edit_response(
                ctx,
                EditInteractionResponse::new().content(format!("AnimeciX local reset failed: {}", e)),
            ).await.ok();
        }
    }
}
