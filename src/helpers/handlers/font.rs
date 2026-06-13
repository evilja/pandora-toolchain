use super::*;

pub async fn handle_font(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/font").await {
        Some(id) => id,
        None => return,
    };

    let attachment = option_attachment(command, "file");
    let has_attachment = attachment.is_some();
    let link = option_trimmed(command, "link");

    if attachment.is_none() && link.is_none() {
        command_error(ctx, command, "Error: provide either `file` or `link`.").await;
        return;
    }

    if command.create_response(ctx, CreateInteractionResponse::Defer(
        CreateInteractionResponseMessage::new().ephemeral(true)
    )).await.is_err() {
        return;
    }

    let zip_bytes = if let Some(a) = attachment {
        let name = a.filename.to_lowercase();
        if !name.ends_with(".zip") {
            font_response(ctx, command, "Error: `file` must be a .zip archive.").await;
            return;
        }
        match a.download().await {
            Ok(b) => b,
            Err(e) => {
                font_response(ctx, command, format!("Failed to download attachment: {}", e)).await;
                return;
            }
        }
    } else {
        let url = link.unwrap();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            font_response(ctx, command, "Error: `link` must be an http(s) URL.").await;
            return;
        }
        let resp = match reqwest::get(&url).await {
            Ok(r) => r,
            Err(e) => {
                font_response(ctx, command, format!("Failed to fetch zip: {}", e)).await;
                return;
            }
        };
        if !resp.status().is_success() {
            font_response(ctx, command, format!("Failed to fetch zip: HTTP {}", resp.status())).await;
            return;
        }
        match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                font_response(ctx, command, format!("Failed to read zip body: {}", e)).await;
                return;
            }
        }
    };

    let dir = std::path::PathBuf::from("DB")
        .join("fontconfig")
        .join(server_id.to_string());
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        font_response(ctx, command, format!("Failed to create font dir: {}", e)).await;
        return;
    }

    let count = match extract_zip_to_dir(&zip_bytes, &dir).await {
        Ok(c) => c,
        Err(e) => {
            font_response(ctx, command, format!("Failed to extract zip: {}", e)).await;
            return;
        }
    };

    let source = if has_attachment { "attachment" } else { "link" };
    font_response(ctx, command, format!("Extracted {} file(s) from {} into `{}`.", count, source, dir.display())).await;
}
