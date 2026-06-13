use super::*;

pub async fn handle_auth(
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
    let level = option_str(command, "level")
        .unwrap_or("authorize.pandora")
        .to_string();

    if !has_level_at_least(command.user.id.get(), level_rank(&level)) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Error: your level does not outrank `{}`.", level))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let path = perm_path(&level);
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
                .content(format!("Authorized <@{}> at `{}`.", user_id, level))
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
