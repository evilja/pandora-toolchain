use super::*;

pub async fn handle_lsauth(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let level = match option_trimmed(command, "level") {
        Some(s) => s,
        None => {
            command_error(ctx, command, "Error: `level` is required.").await;
            return;
        }
    };
    if level_rank(&level) == u8::MAX {
        command_error(ctx, command, "Error: unknown auth level.").await;
        return;
    }
    let mut users = get_perm(perm_path(&level))
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.starts_with(';') && !s.starts_with('#'))
        .collect::<Vec<_>>();
    users.sort();
    users.dedup();
    let rank = level_rank(&level);
    let label = help_rank_label(rank);
    let mut body = if users.is_empty() {
        format!("No users in `{}` (rank {}, {}).", level, rank, label)
    } else {
        let max = 60usize;
        let shown = users.iter()
            .take(max)
            .map(|id| format!("<@{}>", id))
            .collect::<Vec<_>>()
            .join(" ");
        if users.len() > max {
            format!("`{}` rank {} ({}):\n{}\n...and {} more", level, rank, label, shown, users.len() - max)
        } else {
            format!("`{}` rank {} ({}):\n{}", level, rank, label, shown)
        }
    };
    if body.len() > 1900 {
        body.truncate(1900);
        body.push_str("\n...");
    }
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(body)
            .ephemeral(true)
    )).await.ok();
}
