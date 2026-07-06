use super::*;
use pandora_toolchain::lib::env::standard::FLAVOR_PATH;
use pandora_toolchain::pnworker::presence::idle_flavors;
use tokio::io::AsyncWriteExt;

const MAX_FLAVOR_CHARS: usize = 128;

pub async fn handle_touchflavor(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let text = match option_trimmed(command, "text") {
        Some(s) => s,
        None => {
            command_error(ctx, command, "Error: `text` is required.").await;
            return;
        }
    };
    if text.contains('\n') || text.contains('\r') {
        command_error(ctx, command, "Error: flavor text cannot contain newlines.").await;
        return;
    }
    if text.chars().count() > MAX_FLAVOR_CHARS {
        command_error(ctx, command, format!("Error: flavor text must be {} characters or fewer.", MAX_FLAVOR_CHARS)).await;
        return;
    }

    if let Some(parent) = std::path::Path::new(FLAVOR_PATH).parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            command_error(ctx, command, format!("Failed to create flavor dir: {}", e)).await;
            return;
        }
    }

    let write_result = async {
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(FLAVOR_PATH)
            .await?;
        f.write_all(format!("{}\n", text).as_bytes()).await
    }
    .await;

    if let Err(e) = write_result {
        command_error(ctx, command, format!("Failed to write flavor: {}", e)).await;
        return;
    }

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Added idle flavor #{}: `{}`", idle_flavors().await.len(), inline(&text)))
            .ephemeral(true)
    )).await.ok();
}

pub async fn handle_lsflavor(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let flavors = idle_flavors().await;
    if flavors.is_empty() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("No idle flavors are stored.")
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let page_size = 10usize;
    let total_pages = (flavors.len() + page_size - 1) / page_size;
    let requested = option_i64(command, "page").unwrap_or(1).max(1) as usize;
    let page = requested.min(total_pages).max(1);
    let start = (page - 1) * page_size;
    let lines = flavors.iter()
        .enumerate()
        .skip(start)
        .take(page_size)
        .map(|(idx, text)| format!("`{}` `{}`", idx + 1, inline(text)))
        .collect::<Vec<_>>();

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Idle flavors page {}/{}:\n{}", page, total_pages, lines.join("\n")))
            .ephemeral(true)
    )).await.ok();
}

pub async fn handle_rmflavor(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let index = match option_i64(command, "index") {
        Some(i) if i > 0 => i as usize,
        _ => {
            command_error(ctx, command, "Error: `index` must be a positive number from `/lsflavor`.").await;
            return;
        }
    };
    let flavors = idle_flavors().await;
    if index == 0 || index > flavors.len() {
        command_error(ctx, command, format!("Error: no idle flavor #{} exists.", index)).await;
        return;
    }
    let removed = flavors[index - 1].clone();
    let mut updated = flavors;
    updated.remove(index - 1);
    let mut text = updated.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }

    if let Err(e) = tokio::fs::write(FLAVOR_PATH, text).await {
        command_error(ctx, command, format!("Failed to write flavors: {}", e)).await;
        return;
    }

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Removed idle flavor #{}: `{}`", index, inline(&removed)))
            .ephemeral(true)
    )).await.ok();
}

fn inline(s: &str) -> String {
    s.replace('`', "'")
}
