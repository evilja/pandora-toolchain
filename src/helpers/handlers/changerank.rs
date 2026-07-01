use super::*;

pub async fn handle_changerank(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let name = match option_trimmed(command, "command") {
        Some(s) => s.trim_start_matches('/').to_string(),
        None => {
            command_error(ctx, command, "Error: `command` is required.").await;
            return;
        }
    };
    if name == "changerank" {
        command_error(ctx, command, "Error: `/changerank` cannot change its own rank.").await;
        return;
    }
    let rank = match option_i64(command, "rank") {
        Some(n) if (0..=4).contains(&n) => n as u8,
        _ => {
            command_error(ctx, command, "Error: `rank` must be between 0 and 4.").await;
            return;
        }
    };
    match set_command_rank(&name, rank) {
        Ok(()) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Set `{}` to rank {} ({}).", name, rank, help_rank_label(rank)))
                    .ephemeral(true)
            )).await.ok();
        }
        Err(e) => {
            command_error(ctx, command, format!("Failed to change rank: {}", e)).await;
        }
    }
}
