use super::*;

const TOKENS_PATH: &str = pandora_toolchain::libpnenv::standard::API_TOKENS_PATH;

struct TokenEntry {
    token: String,
    label: Option<String>,
    local_server_id: Option<u64>,
}

pub async fn handle_lstoken(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let contents = match tokio::fs::read_to_string(TOKENS_PATH).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            command_error(ctx, command, format!("Failed to read tokens: {}", e)).await;
            return;
        }
    };
    let tokens = parse_token_entries(&contents);
    if tokens.is_empty() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("No API tokens are stored.")
                .ephemeral(true)
        )).await.ok();
        return;
    }
    let page_size = 10usize;
    let total_pages = (tokens.len() + page_size - 1) / page_size;
    let requested = option_i64(command, "page").unwrap_or(1).max(1) as usize;
    let page = requested.min(total_pages).max(1);
    let start = (page - 1) * page_size;
    let mut lines = Vec::new();
    for entry in tokens.iter().skip(start).take(page_size) {
        let label = entry.label.as_deref().unwrap_or("(none)");
        let local = match entry.local_server_id {
            Some(id) => format!("yes ({})", id),
            None => "no".to_string(),
        };
        lines.push(format!(
            "`{}` label:`{}` local:`{}`",
            masked_token(&entry.token),
            inline(label),
            local
        ));
    }
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("API tokens page {}/{}:\n{}", page, total_pages, lines.join("\n")))
            .ephemeral(true)
    )).await.ok();
}

pub async fn handle_rmtoken(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let label = option_trimmed(command, "label");
    let token = option_trimmed(command, "token");
    if label.is_some() == token.is_some() {
        command_error(ctx, command, "Error: provide exactly one of `label` or `token`.").await;
        return;
    }
    let contents = match tokio::fs::read_to_string(TOKENS_PATH).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            command_error(ctx, command, "No API token file exists.").await;
            return;
        }
        Err(e) => {
            command_error(ctx, command, format!("Failed to read tokens: {}", e)).await;
            return;
        }
    };
    let (updated, removed, matched) = if let Some(label) = label {
        let (updated, removed) = remove_labelled_tokens(&contents, &label);
        (updated, removed, format!("label `{}`", inline(&label)))
    } else {
        let token = token.unwrap();
        match remove_masked_token(&contents, &token) {
            Ok((updated, removed)) => (updated, removed, format!("token `{}`", inline(&token))),
            Err(e) => {
                command_error(ctx, command, e).await;
                return;
            }
        }
    };
    if removed == 0 {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("No tokens matched {}.", matched))
                .ephemeral(true)
        )).await.ok();
        return;
    }
    if let Err(e) = tokio::fs::write(TOKENS_PATH, updated).await {
        command_error(ctx, command, format!("Failed to write tokens: {}", e)).await;
        return;
    }
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Removed {} token(s) matching {}.", removed, matched))
            .ephemeral(true)
    )).await.ok();
}

fn parse_token_entries(contents: &str) -> Vec<TokenEntry> {
    let mut out = Vec::new();
    let mut pending_label: Option<String> = None;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with(';') {
            pending_label = parse_label_comment(trimmed);
            continue;
        }
        let mut parts = trimmed.split('|');
        let token = parts.next().unwrap_or("").trim();
        if token.is_empty() {
            pending_label = None;
            continue;
        }
        let local_server_id = match (parts.next(), parts.next()) {
            (Some("local"), Some(server_id)) => server_id.trim().parse::<u64>().ok(),
            _ => None,
        };
        out.push(TokenEntry {
            token: token.to_string(),
            label: pending_label.take(),
            local_server_id,
        });
    }
    out
}

fn remove_labelled_tokens(contents: &str, target: &str) -> (String, usize) {
    let mut out = Vec::new();
    let mut pending = Vec::new();
    let mut pending_label: Option<String> = None;
    let mut removed = 0usize;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(';') {
            pending_label = parse_label_comment(trimmed);
            pending.push(line.to_string());
            continue;
        }
        if trimmed.is_empty() {
            if pending.is_empty() {
                out.push(line.to_string());
            } else {
                pending.push(line.to_string());
            }
            continue;
        }
        let label = pending_label.take();
        if label.as_deref() == Some(target) {
            pending.clear();
            removed += 1;
            continue;
        }
        out.append(&mut pending);
        out.push(line.to_string());
    }
    out.append(&mut pending);
    let mut text = out.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    (text, removed)
}

fn remove_masked_token(contents: &str, target: &str) -> Result<(String, usize), String> {
    let matches = parse_token_entries(contents)
        .into_iter()
        .filter(|entry| masked_token(&entry.token) == target)
        .count();
    if matches == 0 {
        return Err(format!("No token matched `{}`.", inline(target)));
    }
    if matches > 1 {
        return Err(format!("Token mask `{}` matched {} tokens; use labels to disambiguate.", inline(target), matches));
    }

    let mut out = Vec::new();
    let mut pending = Vec::new();
    let mut removed = 0usize;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(';') {
            pending.push(line.to_string());
            continue;
        }
        if trimmed.is_empty() {
            if pending.is_empty() {
                out.push(line.to_string());
            } else {
                pending.push(line.to_string());
            }
            continue;
        }
        let stored = trimmed.split('|').next().unwrap_or("").trim();
        if masked_token(stored) == target {
            pending.clear();
            removed += 1;
            continue;
        }
        out.append(&mut pending);
        out.push(line.to_string());
    }
    out.append(&mut pending);
    let mut text = out.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    Ok((text, removed))
}

fn parse_label_comment(line: &str) -> Option<String> {
    let body = line.trim_start_matches(';').trim();
    if body.is_empty() {
        return None;
    }
    let label = body.split_once(" (added ")
        .map(|(l, _)| l)
        .unwrap_or(body)
        .trim();
    if label.is_empty() {
        None
    } else {
        Some(label.to_string())
    }
}

fn masked_token(token: &str) -> String {
    if token.len() <= 6 {
        token.to_string()
    } else {
        format!("{}...{}", &token[..3], &token[token.len() - 3..])
    }
}

fn inline(s: &str) -> String {
    s.replace('`', "'")
}
