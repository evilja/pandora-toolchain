use super::*;
use tokio::io::AsyncWriteExt;

const TOKENS_PATH: &str = pandora_toolchain::libpnenv::standard::API_TOKENS_PATH;

pub async fn handle_gentoken(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let label = option_trimmed(command, "label");
    let local = option_bool(command, "local").unwrap_or(false);
    let local_server_id = if local {
        match command_server_id(ctx, command, "/gentoken local").await {
            Some(id) => Some(id),
            None => return,
        }
    } else {
        None
    };
    if let Some(l) = &label {
        if l.contains('\n') || l.contains('\r') {
            command_error(ctx, command, "Error: `label` cannot contain newlines.").await;
            return;
        }
    }

    let token = match generate_token() {
        Ok(t) => t,
        Err(e) => {
            command_error(ctx, command, format!("Failed to generate token: {}", e)).await;
            return;
        }
    };

    if let Some(parent) = std::path::Path::new(TOKENS_PATH).parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            command_error(ctx, command, format!("Failed to create token dir: {}", e)).await;
            return;
        }
    }

    let mut blob = String::from("\n");
    if let Some(l) = &label {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        blob.push_str(&format!("; {} (added {})\n", l, ts));
    }
    match local_server_id {
        Some(id) => blob.push_str(&format!("{}|local|{}", token, id)),
        None => blob.push_str(&token),
    }
    blob.push('\n');

    let write_result = async {
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(TOKENS_PATH)
            .await?;
        f.write_all(blob.as_bytes()).await
    }
    .await;

    if let Err(e) = write_result {
        command_error(ctx, command, format!("Failed to write token: {}", e)).await;
        return;
    }

    let labelled = label.map(|l| format!(" for `{}`", l)).unwrap_or_default();
    let scope = local_server_id
        .map(|id| format!(" It will use this server's Google Drive credentials (`{}`) when available, otherwise the global credentials.", id))
        .unwrap_or_default();
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!(
                "Created an API bearer token{}.{} It's stored in `{}` and shown only here:\n```\n{}\n```\nSend it as `Authorization: Bearer <token>`.",
                labelled, scope, TOKENS_PATH, token
            ))
            .ephemeral(true)
    )).await.ok();
}

fn generate_token() -> Result<String, String> {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).map_err(|e| format!("entropy source failed: {}", e))?;
    let mut out = String::with_capacity(64);
    for b in buf {
        out.push_str(&format!("{:02x}", b));
    }
    Ok(out)
}
