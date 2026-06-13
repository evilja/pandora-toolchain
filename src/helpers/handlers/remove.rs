use super::*;

pub async fn handle_remove(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let user_id = match option_trimmed(command, "user_id") {
        Some(s) => s,
        None => {
            command_error(ctx, command, "Error: `user_id` is required.").await;
            return;
        }
    };
    let level = match option_trimmed(command, "level") {
        Some(s) => s.to_string(),
        None => {
            command_error(ctx, command, "Error: `level` is required.").await;
            return;
        }
    };

    if !has_level_at_least(command.user.id.get(), level_rank(&level)) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Error: your level does not outrank `{}`.", level))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    match remove_env(&perm_path(&level), &user_id) {
        Ok(true) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Removed <@{}> from `{}`.", user_id, level))
                    .ephemeral(true)
            )).await.ok();
        }
        Ok(false) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("<@{}> was not in `{}`.", user_id, level))
                    .ephemeral(true)
            )).await.ok();
        }
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to remove: {}", e))
                    .ephemeral(true)
            )).await.ok();
        }
    }
}
