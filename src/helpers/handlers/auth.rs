use super::*;

pub async fn handle_auth(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    // Permission files hold bare numeric ids; a pasted mention or name is the common mistake.
    let Some(user_id) = option_trimmed(command, "user_id").and_then(|id| parse_discord_user_id(&id)) else {
        command_error(ctx, command, "Error: `user_id` must be a numeric Discord user id (right-click the user → Copy User ID), not a name or mention.").await;
        return;
    };
    let level = option_str(command, "level")
        .unwrap_or("authorize.pandora")
        .to_string();

    if level_rank(&level) == u8::MAX {
        command_error(ctx, command, "Error: unknown auth level. Pick one of authorize, fansubber, admin, upper, witch.").await;
        return;
    }
    if !has_level_at_least(command.user.id.get(), level_rank(&level)) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Error: you can only grant tiers at or below your own. `{}` needs {} rank or higher.", level, help_rank_label(level_rank(&level))))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let path = perm_path(&level);
    // Adding the same id twice used to append a duplicate line; say so instead.
    if get_perm(path.clone()).iter().any(|id| id == &user_id) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .embed(info_embed(command, COMMAND_LIST)
                    .description(format!("<@{}> is already authorized at `{}`.", user_id, level)))
                .ephemeral(true)
        )).await.ok();
        return;
    }
    if let Some(parent) = std::path::Path::new(&path).parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to authorize: could not create permission dir: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    }
    if let Err(e) = tokio::fs::OpenOptions::new().create(true).append(true).open(&path).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to authorize: could not create `{}`: {}", level, e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let mut to_add = user_id.clone();
    if add_env(&path, &mut to_add) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .embed(success_embed(command, COMMAND_UPDATED)
                    .description(format!("Authorized <@{}> at `{}`.", user_id, level)))
                .ephemeral(true)
        )).await.ok();
    } else {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to authorize: could not open `{}` for writing.", level))
                .ephemeral(true)
        )).await.ok();
    }
}
