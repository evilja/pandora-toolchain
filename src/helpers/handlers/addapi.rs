use super::*;

pub async fn handle_addapi(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let key_name = match option_trimmed(command, "key_name") {
        Some(s) => s,
        None => {
            command_error(ctx, command, "Error: `key_name` is required.").await;
            return;
        }
    };
    let token = match option_trimmed(command, "token") {
        Some(s) => s,
        None => {
            command_error(ctx, command, "Error: `token` is required.").await;
            return;
        }
    };

    if key_name.contains('\n') || key_name.contains('\r') || key_name.contains('|') {
        command_error(ctx, command, "Error: `key_name` cannot contain newlines or `|`.").await;
        return;
    }
    if token.contains('\n') || token.contains('\r') {
        command_error(ctx, command, "Error: `token` cannot contain newlines.").await;
        return;
    }

    if let Some(parent) = std::path::Path::new(ENV_PATH).parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to create env dir: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    }

    let existed = match upsert_env(ENV_PATH, &key_name, &token) {
        Ok(updated) => updated,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to write env entry: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let action = if existed { "Updated" } else { "Added" };
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("{} `{}` in `{}`.", action, key_name, ENV_PATH))
            .ephemeral(true)
    )).await.ok();
}
