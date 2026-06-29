use super::*;
use pandora_toolchain::pnworker::messages::{get_arg_count, get_message, init_language_files, MessageEntry};
use serenity::builder::CreateAttachment;

fn translation_lang(command: &serenity::all::CommandInteraction) -> Option<String> {
    option_str(command, "language")
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .filter(|s| matches!(s.as_str(), "en" | "tr" | "jp"))
}

fn translation_key(command: &serenity::all::CommandInteraction) -> Option<String> {
    option_str(command, "key")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_uppercase)
}

fn translation_path(lang: &str) -> std::path::PathBuf {
    std::path::PathBuf::from("DB").join("config").join(format!("{}.toml", lang))
}

fn infer_arg_count(text: &str) -> usize {
    text.match_indices("{}").count()
}

fn truncate_discord(s: String) -> String {
    if s.len() <= 1900 {
        return s;
    }
    let mut out: String = s.chars().take(1800).collect();
    out.push_str("\n...");
    out
}

pub async fn handle_gettranslation(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    init_language_files();
    let lang = match translation_lang(command) {
        Some(v) => v,
        None => {
            command_error(ctx, command, "Error: language must be en, tr, or jp").await;
            return;
        }
    };
    let key = match translation_key(command) {
        Some(v) => v,
        None => {
            command_error(ctx, command, "Error: key is required").await;
            return;
        }
    };
    let args = match get_arg_count(&key, &lang) {
        Some(v) => v,
        None => {
            command_error(ctx, command, format!("Error: translation key `{}` was not found", key)).await;
            return;
        }
    };
    let text = get_message(&key, &lang);
    let body = truncate_discord(format!(
        "Translation `{}` / `{}`\nargs: `{}`\n```text\n{}\n```",
        lang, key, args, text
    ));
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(body)
            .ephemeral(true)
    )).await.ok();
}

pub async fn handle_addtranslation(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    init_language_files();
    let lang = match translation_lang(command) {
        Some(v) => v,
        None => {
            command_error(ctx, command, "Error: language must be en, tr, or jp").await;
            return;
        }
    };
    let key = match translation_key(command) {
        Some(v) => v,
        None => {
            command_error(ctx, command, "Error: key is required").await;
            return;
        }
    };
    let text = match option_str(command, "text").map(str::to_string).filter(|s| !s.is_empty()) {
        Some(v) => v,
        None => {
            command_error(ctx, command, "Error: text is required").await;
            return;
        }
    };
    let args = option_i64(command, "args")
        .map(|v| v.max(0) as usize)
        .or_else(|| get_arg_count(&key, &lang))
        .unwrap_or_else(|| infer_arg_count(&text));

    let path = translation_path(&lang);
    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            command_error(ctx, command, format!("Failed to create config dir: {}", e)).await;
            return;
        }
    }
    let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let mut map = if content.trim().is_empty() {
        std::collections::BTreeMap::<String, MessageEntry>::new()
    } else {
        match toml::from_str::<std::collections::BTreeMap<String, MessageEntry>>(&content) {
            Ok(v) => v,
            Err(e) => {
                command_error(ctx, command, format!("Failed to parse {}: {}", path.display(), e)).await;
                return;
            }
        }
    };
    let existed = map.contains_key(&key) || get_arg_count(&key, &lang).is_some();
    map.insert(key.clone(), MessageEntry { text, args });
    let body = match toml::to_string_pretty(&map) {
        Ok(v) => v,
        Err(e) => {
            command_error(ctx, command, format!("Failed to serialize translations: {}", e)).await;
            return;
        }
    };
    if let Err(e) = tokio::fs::write(&path, body).await {
        command_error(ctx, command, format!("Failed to write {}: {}", path.display(), e)).await;
        return;
    }
    let verb = if existed { "Updated" } else { "Added" };
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("{} translation `{}` / `{}` with args `{}`.", verb, lang, key, args))
            .ephemeral(true)
    )).await.ok();
}

pub async fn handle_gettranslationall(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    init_language_files();
    let lang = match translation_lang(command) {
        Some(v) => v,
        None => {
            command_error(ctx, command, "Error: language must be en, tr, or jp").await;
            return;
        }
    };
    let path = translation_path(&lang);
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(v) => v,
        Err(e) => {
            command_error(ctx, command, format!("Failed to read {}: {}", path.display(), e)).await;
            return;
        }
    };
    let file = CreateAttachment::bytes(content.into_bytes(), format!("{}.toml", lang));
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Current `{}` translations.", lang))
            .add_file(file)
            .ephemeral(true)
    )).await.ok();
}

pub async fn handle_addtranslationall(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    init_language_files();
    let lang = match translation_lang(command) {
        Some(v) => v,
        None => {
            command_error(ctx, command, "Error: language must be en, tr, or jp").await;
            return;
        }
    };
    let attachment = match option_attachment(command, "file") {
        Some(a) => a,
        None => {
            command_error(ctx, command, "Error: `file` attachment is required.").await;
            return;
        }
    };
    if !attachment.filename.to_ascii_lowercase().ends_with(".toml") {
        command_error(ctx, command, "Error: `file` must be a .toml file.").await;
        return;
    }
    let bytes = match attachment.download().await {
        Ok(v) => v,
        Err(e) => {
            command_error(ctx, command, format!("Failed to download attachment: {}", e)).await;
            return;
        }
    };
    let content = match String::from_utf8(bytes) {
        Ok(v) => v,
        Err(e) => {
            command_error(ctx, command, format!("TOML must be UTF-8: {}", e)).await;
            return;
        }
    };
    let map = match toml::from_str::<std::collections::BTreeMap<String, MessageEntry>>(&content) {
        Ok(v) => v,
        Err(e) => {
            command_error(ctx, command, format!("Failed to parse TOML: {}", e)).await;
            return;
        }
    };
    if map.is_empty() {
        command_error(ctx, command, "Error: TOML has no translation entries.").await;
        return;
    }
    let body = match toml::to_string_pretty(&map) {
        Ok(v) => v,
        Err(e) => {
            command_error(ctx, command, format!("Failed to serialize translations: {}", e)).await;
            return;
        }
    };
    let path = translation_path(&lang);
    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            command_error(ctx, command, format!("Failed to create config dir: {}", e)).await;
            return;
        }
    }
    if let Err(e) = tokio::fs::write(&path, body).await {
        command_error(ctx, command, format!("Failed to write {}: {}", path.display(), e)).await;
        return;
    }
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Replaced `{}` translations with {} entries from `{}`.", lang, map.len(), attachment.filename))
            .ephemeral(true)
    )).await.ok();
}
