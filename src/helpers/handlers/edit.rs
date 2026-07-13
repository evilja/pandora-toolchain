use super::*;
use serenity::builder::CreateAutocompleteResponse;

const CLEAR_SENTINEL: &str = "-";
const DISABLE_CONCAT_LABEL: &str = "Disable concat";
const MAX_CONCAT_CHOICES: usize = 25;
const MAX_CONCAT_GROUP_CHOICES: usize = MAX_CONCAT_CHOICES - 1;
const MAX_CONCAT_CHOICE_CHARS: usize = 100;

fn filter_concat_choices(
    groups: &std::collections::HashMap<String, Vec<String>>,
    partial: &str,
) -> Vec<(String, String)> {
    let partial = partial.to_lowercase();
    let mut names = groups
        .keys()
        .filter(|name| {
            !name.trim().is_empty()
                && name.as_str() != CLEAR_SENTINEL
                && name.chars().count() <= MAX_CONCAT_CHOICE_CHARS
                && name.to_lowercase().contains(&partial)
        })
        .cloned()
        .collect::<Vec<_>>();
    names.sort_by(|left, right| {
        left.to_lowercase()
            .cmp(&right.to_lowercase())
            .then_with(|| left.cmp(right))
    });

    let mut choices = vec![(DISABLE_CONCAT_LABEL.to_string(), CLEAR_SENTINEL.to_string())];
    choices.extend(
        names
            .into_iter()
            .take(MAX_CONCAT_GROUP_CHOICES)
            .map(|name| (name.clone(), name)),
    );
    choices
}

pub async fn handle_edit_autocomplete(
    ctx: &Context,
    interaction: &serenity::all::CommandInteraction,
) {
    let partial = interaction
        .data
        .autocomplete()
        .filter(|option| option.name == "concat")
        .map(|option| option.value.to_string())
        .unwrap_or_default();
    let config = IntrosConfig::load();
    let mut response = CreateAutocompleteResponse::new();
    for (label, value) in filter_concat_choices(&config.groups, &partial) {
        response = response.add_string_choice(label, value);
    }
    if let Err(e) = interaction
        .create_response(ctx, CreateInteractionResponse::Autocomplete(response))
        .await
    {
        eprintln!("[edit] concat autocomplete response failed: {}", e);
    }
}

fn edit_text_field(command: &serenity::all::CommandInteraction, name: &str, existing: &str) -> String {
    match option_str(command, name).map(str::trim) {
        None => existing.to_string(),
        Some(CLEAR_SENTINEL) => String::new(),
        Some(s) if s.is_empty() => existing.to_string(),
        Some(s) => s.to_string(),
    }
}

pub async fn handle_edit(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/edit").await {
        Some(id) => id,
        None => return,
    };

    let dir = std::path::PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string());

    let existing_meta = std::fs::read_to_string(dir.join("meta.pandora")).unwrap_or_default();
    if existing_meta.trim().is_empty() {
        command_error(ctx, command, "Error: this server has no config yet. Run /configure first.").await;
        return;
    }
    let existing_lines: Vec<&str> = existing_meta.lines().collect();
    let existing_language = existing_lines.get(0).copied().unwrap_or("");
    let existing_forgejo = existing_lines.get(1).copied().unwrap_or("");
    let existing_channel = existing_lines.get(2).copied().unwrap_or("");
    let existing_api_key = existing_lines.get(3).copied().unwrap_or("");
    let existing_gdrive_client_id = existing_lines.get(4).copied().unwrap_or("");
    let existing_gdrive_client_secret = existing_lines.get(5).copied().unwrap_or("");
    let existing_gdrive_refresh_token = existing_lines.get(6).copied().unwrap_or("");
    let existing_gdrive_folder_id = existing_lines.get(7).copied().unwrap_or("");
    let existing_wrap_style = existing_lines.get(8).copied().unwrap_or("");
    let existing_local_gdrive = existing_lines.get(9).copied().unwrap_or("true");
    let existing_gdrive_anon_folder_id = existing_lines.get(10).copied().unwrap_or("");
    let existing_preset = existing_lines.get(11).copied().unwrap_or("standard");
    let existing_concat = existing_lines.get(12).copied().unwrap_or("");

    let language = match option_str(command, "language") {
        Some(l) if matches!(l, "EN" | "TR" | "JP") => l.to_string(),
        Some(other) => {
            command_error(ctx, command, format!("Error: language `{}` is not one of EN/TR/JP", other)).await;
            return;
        }
        None => existing_language.to_string(),
    };

    let forgejo = match option_str(command, "forgejo").map(str::trim) {
        None => existing_forgejo.to_string(),
        Some(CLEAR_SENTINEL) => String::new(),
        Some(u) if u.is_empty() => existing_forgejo.to_string(),
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.trim_end_matches('/').to_string(),
        Some(other) => {
            command_error(ctx, command, format!("Error: forgejo `{}` must be an http(s) URL", other)).await;
            return;
        }
    };

    let channel = match option_bool(command, "announcement_channel") {
        Some(true) => command.channel_id.get().to_string(),
        _ => existing_channel.to_string(),
    };

    let new_api_key = edit_text_field(command, "api_key", existing_api_key);
    let gdrive_client_id = edit_text_field(command, "gdrive_client_id", existing_gdrive_client_id);
    let gdrive_client_secret = edit_text_field(command, "gdrive_client_secret", existing_gdrive_client_secret);
    let gdrive_refresh_token = edit_text_field(command, "gdrive_refresh_token", existing_gdrive_refresh_token);
    let gdrive_folder_id = edit_text_field(command, "gdrive_folder_id", existing_gdrive_folder_id);
    let gdrive_anon_folder_id = edit_text_field(command, "gdrive_anon_folder_id", existing_gdrive_anon_folder_id);
    let wrap_style = match option_str(command, "wrapstyle").map(str::trim) {
        None => existing_wrap_style.to_string(),
        Some("dont_touch") | Some("keep") | Some(CLEAR_SENTINEL) => String::new(),
        Some(v) if matches!(v, "0" | "1" | "2" | "3") => v.to_string(),
        Some(other) => {
            command_error(ctx, command, format!("Error: wrapstyle `{}` must be dont_touch, 0, 1, 2, or 3", other)).await;
            return;
        }
    };
    let local_gdrive = option_bool(command, "local_gdrive")
        .map(|v| if v { "true" } else { "false" }.to_string())
        .unwrap_or_else(|| existing_local_gdrive.to_string());
    let preset = match option_str(command, "preset").map(str::trim) {
        None => existing_preset.to_string(),
        Some("standard") | Some("gpu") | Some("pseudolossless") | Some("dummy") => option_str(command, "preset").unwrap().to_string(),
        Some(other) => {
            command_error(ctx, command, format!("Error: preset `{}` is not standard, gpu, pseudolossless, or dummy", other)).await;
            return;
        }
    };
    let concat = match option_str(command, "concat").map(str::trim) {
        None => existing_concat.to_string(),
        Some("-") | Some("") => String::new(),
        Some(group) if IntrosConfig::load().resolve(group).is_some() => group.to_string(),
        Some(group) => {
            command_error(ctx, command, format!("Error: concat group `{}` does not exist", group)).await;
            return;
        }
    };
    let gdrive_auth_parts = [&gdrive_client_id, &gdrive_client_secret, &gdrive_refresh_token];
    let any_gdrive = gdrive_auth_parts.iter().any(|s| !s.is_empty())
        || !gdrive_folder_id.is_empty()
        || !gdrive_anon_folder_id.is_empty();
    if any_gdrive
        && (gdrive_auth_parts.iter().any(|s| s.is_empty())
            || (gdrive_folder_id.is_empty() && gdrive_anon_folder_id.is_empty()))
    {
        command_error(ctx, command, "Error: Google Drive config requires client id, client secret, refresh token, and at least one folder id.").await;
        return;
    }

    let body = format!("{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n", language, forgejo, channel, new_api_key, gdrive_client_id, gdrive_client_secret, gdrive_refresh_token, gdrive_folder_id, wrap_style, local_gdrive, gdrive_anon_folder_id, preset, concat);
    let path = dir.join("meta.pandora");
    if let Err(e) = tokio::fs::write(&path, body).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to write meta.pandora: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let forgejo_display = if forgejo.is_empty() { "(unset)".to_string() } else { format!("`{}`", forgejo) };
    let api_key_display = if new_api_key.is_empty() { "(unset)".to_string() } else { "(set)".to_string() };
    let gdrive_display = if gdrive_client_id.is_empty() && gdrive_client_secret.is_empty() && gdrive_refresh_token.is_empty() && gdrive_folder_id.is_empty() && gdrive_anon_folder_id.is_empty() { "(unset)".to_string() } else { "(set)".to_string() };
    let gdrive_anon_display = if gdrive_anon_folder_id.is_empty() { "(unset)".to_string() } else { "(set)".to_string() };
    let channel_display = if channel.is_empty() { "(unset)".to_string() } else { format!("<#{}>", channel) };
    let wrap_display = if wrap_style.is_empty() { "dont_touch".to_string() } else { wrap_style.clone() };
    let local_gdrive_display = if local_gdrive == "false" { "disabled" } else { "enabled" };
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Edited server `{}` — language: {}, forgejo: {}, forgejo api_key: {}, gdrive: {}, gdrive_anon_folder_id: {}, local_gdrive: {}, wrapstyle: {}, preset: {}, concat: {}, announcement channel: {}",
                server_id, language, forgejo_display, api_key_display, gdrive_display, gdrive_anon_display, local_gdrive_display, wrap_display, preset, if concat.is_empty() { "(disabled)" } else { &concat }, channel_display))
            .ephemeral(true)
    )).await.ok();
}

#[cfg(test)]
mod tests {
    use super::{filter_concat_choices, CLEAR_SENTINEL, DISABLE_CONCAT_LABEL, MAX_CONCAT_CHOICES};
    use std::collections::HashMap;

    fn groups(names: &[&str]) -> HashMap<String, Vec<String>> {
        names
            .iter()
            .map(|name| ((*name).to_string(), Vec::new()))
            .collect()
    }

    #[test]
    fn empty_input_lists_groups_alphabetically() {
        let choices = filter_concat_choices(&groups(&["Beta", "Alpha", "Gamma"]), "");
        assert_eq!(
            choices,
            vec![
                (DISABLE_CONCAT_LABEL.to_string(), CLEAR_SENTINEL.to_string()),
                ("Alpha".to_string(), "Alpha".to_string()),
                ("Beta".to_string(), "Beta".to_string()),
                ("Gamma".to_string(), "Gamma".to_string()),
            ]
        );
    }

    #[test]
    fn matching_is_case_insensitive() {
        let choices = filter_concat_choices(&groups(&["Summer Intro", "Winter Intro"]), "SUMMER");
        assert_eq!(
            choices,
            vec![
                (DISABLE_CONCAT_LABEL.to_string(), CLEAR_SENTINEL.to_string()),
                ("Summer Intro".to_string(), "Summer Intro".to_string()),
            ]
        );
    }

    #[test]
    fn disable_choice_is_always_present() {
        let choices = filter_concat_choices(&HashMap::new(), "missing");
        assert_eq!(choices, vec![(DISABLE_CONCAT_LABEL.to_string(), CLEAR_SENTINEL.to_string())]);
    }

    #[test]
    fn choices_are_capped_at_discord_limit() {
        let names = (0..40)
            .map(|idx| format!("Group {:02}", idx))
            .collect::<Vec<_>>();
        let groups = names
            .iter()
            .map(|name| (name.clone(), Vec::new()))
            .collect::<HashMap<_, Vec<String>>>();
        let choices = filter_concat_choices(&groups, "group");
        assert_eq!(choices.len(), MAX_CONCAT_CHOICES);
        assert_eq!(choices[0], (DISABLE_CONCAT_LABEL.to_string(), CLEAR_SENTINEL.to_string()));
        assert_eq!(choices[24].0, "Group 23");
    }

    #[test]
    fn invalid_group_names_are_excluded() {
        let long_name = "x".repeat(101);
        let choices = filter_concat_choices(&groups(&["", "   ", "Valid"]), "");
        let mut malformed = HashMap::new();
        malformed.insert(long_name, Vec::new());
        malformed.insert("Valid".to_string(), Vec::new());
        malformed.insert("".to_string(), Vec::new());
        malformed.insert("\t".to_string(), Vec::new());
        malformed.insert(CLEAR_SENTINEL.to_string(), Vec::new());
        assert_eq!(
            filter_concat_choices(&malformed, ""),
            vec![
                (DISABLE_CONCAT_LABEL.to_string(), CLEAR_SENTINEL.to_string()),
                ("Valid".to_string(), "Valid".to_string()),
            ]
        );
        assert_eq!(choices[0], (DISABLE_CONCAT_LABEL.to_string(), CLEAR_SENTINEL.to_string()));
    }
}
