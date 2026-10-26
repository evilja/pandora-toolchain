use super::*;

pub async fn handle_remove(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    // Permission files hold bare numeric ids; a pasted mention or name is the common mistake.
    let Some(user_id) = option_trimmed(command, "user_id").and_then(|id| parse_discord_user_id(&id)) else {
        command_error(ctx, command, "Error: `user_id` must be a numeric Discord user id (right-click the user → Copy User ID), not a name or mention.").await;
        return;
    };
    let level = match option_trimmed(command, "level") {
        Some(s) => s.to_string(),
        None => {
            command_error(ctx, command, "Error: `level` is required. Pick one of authorize, fansubber, admin, upper, witch.").await;
            return;
        }
    };
    if level_rank(&level) == u8::MAX {
        command_error(ctx, command, "Error: unknown auth level. Pick one of authorize, fansubber, admin, upper, witch.").await;
        return;
    }
    if !has_level_at_least(command.user.id.get(), level_rank(&level)) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Error: you can only remove tiers at or below your own. `{}` needs {} rank or higher.", level, help_rank_label(level_rank(&level))))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    match remove_env(&perm_path(&level), &user_id) {
        Ok(true) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .embed(success_embed(command, COMMAND_UPDATED)
                        .description(format!("Removed <@{}> from `{}`.", user_id, level)))
                    .ephemeral(true)
            )).await.ok();
        }
        Ok(false) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .embed(info_embed(command, COMMAND_LIST)
                        .description(format!("<@{}> was not in `{}`.", user_id, level)))
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
