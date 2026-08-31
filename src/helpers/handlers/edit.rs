use super::*;
use pandora_toolchain::lib::mpeg::hls::{validate_name_template, DEFAULT_NAME_TEMPLATE};
use pandora_toolchain::pnworker::server_config::{
    drive_only_from_meta, fansub_from_meta, hls_from_meta, hls_name_from_meta,
    validate_preset_delivery, FansubSite,
};
use serenity::builder::CreateAutocompleteResponse;

const CLEAR_SENTINEL: &str = "-";
const DISABLE_CONCAT_LABEL: &str = "Disable concat";
const MAX_CONCAT_CHOICES: usize = 25;
const MAX_CONCAT_GROUP_CHOICES: usize = MAX_CONCAT_CHOICES - 1;
const MAX_CONCAT_CHOICE_CHARS: usize = 100;

fn filter_concat_choices(
    groups: &std::collections::HashMap<String, String>,
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

// `/edit` autocompletes several unrelated options, so the focused option decides which directory is
// searched: the local intro groups for `concat`, and the matching site's live fansub directory for
// each per-site fansub selector.
pub async fn handle_edit_autocomplete(
    ctx: &Context,
    interaction: &serenity::all::CommandInteraction,
) {
    let Some(focused) = interaction.data.autocomplete() else {
        return;
    };
    let partial = focused.value.to_string();
    if let Some(site) = FansubSite::from_option_name(focused.name) {
        fansub_autocomplete(ctx, interaction, site, &partial).await;
        return;
    }
    let mut response = CreateAutocompleteResponse::new();
    if focused.name == "concat" {
        let config = IntrosConfig::load();
        for (label, value) in filter_concat_choices(&config.groups, &partial) {
            response = response.add_string_choice(label, value);
        }
    }
    if let Err(e) = interaction
        .create_response(ctx, CreateInteractionResponse::Autocomplete(response))
        .await
    {
        eprintln!("[edit] concat autocomplete response failed: {}", e);
    }
}

// A submitted fansub is re-resolved against the site's live directory so the stored value is always
// a real identifier for that site; `-` clears the selection and an omitted option keeps it.
async fn edit_fansub_field(
    command: &serenity::all::CommandInteraction,
    site: FansubSite,
    existing: Option<String>,
) -> Result<(String, Option<String>), String> {
    let existing_value = existing.unwrap_or_default();
    match option_str(command, site.option_name()).map(str::trim) {
        None => Ok((existing_value, None)),
        Some(CLEAR_SENTINEL) => Ok((String::new(), None)),
        Some(value) if value.is_empty() => Ok((existing_value, None)),
        Some(value) => resolve_fansub_selection(site, value)
            .await
            .map(|option| (option.value.clone(), Some(option.display())))
            .map_err(|e| format!("Error: {}", e)),
    }
}

fn fansub_field_id(site: FansubSite) -> &'static str {
    match site {
        FansubSite::AnimeciX => FIELD_ANIMECIX_FANSUB,
        FansubSite::OpenAnime => FIELD_OPENANIME_FANSUB,
        FansubSite::Anizm => FIELD_ANIZM_FANSUB,
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

    // Resolving a fansub selection contacts that site, which can outlast Discord's three-second
    // initial-response window, so `/edit` defers only when a fansub option was actually submitted.
    let deferred = FansubSite::ALL
        .into_iter()
        .any(|site| option_str(command, site.option_name()).is_some());
    if deferred
        && command
            .create_response(
                ctx,
                CreateInteractionResponse::Defer(
                    CreateInteractionResponseMessage::new().ephemeral(true),
                ),
            )
            .await
            .is_err()
    {
        return;
    }

    let dir = std::path::PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string());

    let existing_meta = std::fs::read_to_string(dir.join("meta.pandora")).unwrap_or_default();
    if existing_meta.trim().is_empty() {
        edit_error(ctx, command, deferred, "Error: this server has no config yet. Run /configure first.").await;
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
    let existing_drive_only = drive_only_from_meta(&existing_meta);
    let existing_hls = hls_from_meta(&existing_meta);
    let existing_hls_name = hls_name_from_meta(&existing_meta);

    let language = match option_str(command, "language") {
        Some(l) if matches!(l, "EN" | "TR" | "JP") => l.to_string(),
        Some(other) => {
            edit_error(ctx, command, deferred, format!("Error: language `{}` is not one of EN/TR/JP", other)).await;
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
            edit_error(ctx, command, deferred, format!("Error: forgejo `{}` must be an http(s) URL", other)).await;
            return;
        }
    };

    let channel = match option_bool(command, "announcement_channel") {
        Some(true) => command.channel_id.get().to_string(),
        _ => existing_channel.to_string(),
    };

    let new_api_key = edit_text_field(command, "api_key", existing_api_key);
    let gdrive_client_id = existing_gdrive_client_id.to_string();
    let gdrive_client_secret = existing_gdrive_client_secret.to_string();
    let gdrive_refresh_token = existing_gdrive_refresh_token.to_string();
    let gdrive_folder_id = existing_gdrive_folder_id.to_string();
    let gdrive_anon_folder_id = existing_gdrive_anon_folder_id.to_string();
    let wrap_style = match option_str(command, "wrapstyle").map(str::trim) {
        None => existing_wrap_style.to_string(),
        Some("dont_touch") | Some("keep") | Some(CLEAR_SENTINEL) => String::new(),
        Some(v) if matches!(v, "0" | "1" | "2" | "3") => v.to_string(),
        Some(other) => {
            edit_error(ctx, command, deferred, format!("Error: wrapstyle `{}` must be dont_touch, 0, 1, 2, or 3", other)).await;
            return;
        }
    };
    let local_gdrive = option_bool(command, "local_gdrive")
        .map(|v| if v { "true" } else { "false" }.to_string())
        .unwrap_or_else(|| existing_local_gdrive.to_string());
    let drive_only = option_bool(command, "drive_only").unwrap_or(existing_drive_only);
    let hls = option_bool(command, "hls").unwrap_or(existing_hls);
    // The template is stored as written, not as rendered: `-` restores the default rather than
    // clearing the line to nothing, because a name is not something a release can go without.
    let hls_name = match option_str(command, "hls_name").map(str::trim) {
        None => existing_hls_name,
        Some(CLEAR_SENTINEL) | Some("") => DEFAULT_NAME_TEMPLATE.to_string(),
        Some(template) => match validate_name_template(template) {
            Ok(template) => template,
            Err(reason) => {
                edit_error(ctx, command, deferred, format!("Error: hls_name {reason}")).await;
                return;
            }
        },
    };
    let preset = match option_str(command, "preset").map(str::trim) {
        None => existing_preset.to_string(),
        Some("standard") | Some("veryslow") | Some("gpu") | Some("av1") | Some("pseudolossless") | Some("dummy") => option_str(command, "preset").unwrap().to_string(),
        Some(other) => {
            edit_error(ctx, command, deferred, format!("Error: preset `{}` is not standard, veryslow, gpu, av1, pseudolossless, or dummy", other)).await;
            return;
        }
    };
    if let Err(reason) = validate_preset_delivery(&preset, drive_only, hls) {
        edit_error(ctx, command, deferred, format!("Error: {reason}")).await;
        return;
    }
    let concat = match option_str(command, "concat").map(str::trim) {
        None => existing_concat.to_string(),
        Some("-") | Some("") => String::new(),
        Some(group) if IntrosConfig::load().resolve(group).is_some() => group.to_string(),
        Some(group) => {
            edit_error(ctx, command, deferred, format!("Error: concat group `{}` does not exist", group)).await;
            return;
        }
    };

    let mut fansubs = Vec::new();
    for site in FansubSite::ALL {
        match edit_fansub_field(command, site, fansub_from_meta(&existing_meta, site)).await {
            Ok(resolved) => fansubs.push((site, resolved)),
            Err(e) => {
                edit_error(ctx, command, deferred, e).await;
                return;
            }
        }
    }
    let fansub_value = |site: FansubSite| {
        fansubs
            .iter()
            .find(|(candidate, _)| *candidate == site)
            .map(|(_, (value, _))| value.clone())
            .unwrap_or_default()
    };

    let body = compose_server_meta(&ServerMetaFields {
        language: language.clone(),
        forgejo: forgejo.clone(),
        announcement_channel: channel.clone(),
        api_key: new_api_key.clone(),
        gdrive_client_id,
        gdrive_client_secret,
        gdrive_refresh_token,
        gdrive_folder_id,
        wrap_style: wrap_style.clone(),
        local_gdrive: local_gdrive.clone(),
        gdrive_anon_folder_id,
        preset: preset.clone(),
        concat: concat.clone(),
        animecix_fansub: fansub_value(FansubSite::AnimeciX),
        drive_only: drive_only.to_string(),
        openanime_fansub: fansub_value(FansubSite::OpenAnime),
        anizm_fansub: fansub_value(FansubSite::Anizm),
        hls: hls.to_string(),
        hls_name: hls_name.clone(),
    });
    let path = dir.join("meta.pandora");
    if let Err(e) = tokio::fs::write(&path, body).await {
        edit_error(ctx, command, deferred, format!("Failed to write meta.pandora: {}", e)).await;
        return;
    }

    let set = command_message(command, VALUE_SET);
    let unset = command_message(command, VALUE_UNSET);
    let forgejo_display = if forgejo.is_empty() { unset.clone() } else { forgejo };
    let api_key_display = if new_api_key.is_empty() { unset.clone() } else { set.clone() };
    let broker_status = match LumiereClient::from_env() {
        Ok(client) => client
            .provider_status(Some(&guild_drive_profile(server_id)))
            .await
            .unwrap_or_default(),
        Err(_) => Default::default(),
    };
    let gdrive_display = if broker_status.requested_drive || broker_status.global_drive { set.clone() } else { unset.clone() };
    let gdrive_anon_display = if broker_status.requested_drive { set.clone() } else { unset.clone() };
    let channel_display = if channel.is_empty() { unset } else { format!("<#{}>", channel) };
    let wrap_display = if wrap_style.is_empty() { "dont_touch".to_string() } else { wrap_style.clone() };
    let local_gdrive_display = if local_gdrive == "false" {
        command_message(command, VALUE_DISABLED)
    } else {
        command_message(command, VALUE_ENABLED)
    };
    let concat_display = if concat.is_empty() {
        command_message(command, VALUE_DISABLED)
    } else {
        concat
    };
    let drive_only_display = command_message(command, if drive_only { VALUE_ENABLED } else { VALUE_DISABLED });
    let hls_display = command_message(command, if hls { VALUE_ENABLED } else { VALUE_DISABLED });
    let mut embed = success_embed(command, COMMAND_SERVER_UPDATED)
        .description(format!("Server `{}`", server_id))
        .field(command_message(command, FIELD_LANGUAGE), language, true)
        .field(command_message(command, FIELD_REPO), forgejo_display, true)
        .field(command_message(command, FIELD_API_KEY), api_key_display, true)
        .field(command_message(command, FIELD_GDRIVE), gdrive_display, true)
        .field(command_message(command, FIELD_GDRIVE_ANONYMOUS), gdrive_anon_display, true)
        .field(command_message(command, FIELD_LOCAL_GDRIVE), local_gdrive_display, true)
        .field(command_message(command, FIELD_DRIVE_ONLY), drive_only_display, true)
        .field(command_message(command, FIELD_HLS), hls_display, true)
        .field(command_message(command, FIELD_HLS_NAME), format!("`{}`", hls_name), true)
        .field(command_message(command, FIELD_WRAPSTYLE), wrap_display, true)
        .field(command_message(command, FIELD_PRESET), preset, true)
        .field(command_message(command, FIELD_CONCAT), concat_display, true);
    for (site, (value, display)) in &fansubs {
        let shown = display
            .clone()
            .or_else(|| Some(value.clone()).filter(|value| !value.is_empty()))
            .unwrap_or_else(|| command_message(command, VALUE_UNSET));
        embed = embed.field(command_message(command, fansub_field_id(*site)), shown, true);
    }
    let embed = embed.field(command_message(command, FIELD_ANNOUNCEMENT), channel_display, false);
    if deferred {
        command
            .edit_response(ctx, EditInteractionResponse::new().embed(embed))
            .await
            .ok();
    } else {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .embed(embed)
                .ephemeral(true)
        )).await.ok();
    }
}

// `command_error` opens a fresh response, which Discord rejects once the interaction was deferred.
async fn edit_error(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    deferred: bool,
    content: impl Into<String>,
) {
    if deferred {
        command
            .edit_response(ctx, EditInteractionResponse::new().content(content.into()))
            .await
            .ok();
    } else {
        command_error(ctx, command, content).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{filter_concat_choices, CLEAR_SENTINEL, DISABLE_CONCAT_LABEL, MAX_CONCAT_CHOICES};
    use std::collections::HashMap;

    fn groups(names: &[&str]) -> HashMap<String, String> {
        names
            .iter()
            .map(|name| ((*name).to_string(), format!("DB/concat/{}", name)))
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
            .map(|name| (name.clone(), format!("DB/concat/{}", name)))
            .collect::<HashMap<_, String>>();
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
        malformed.insert(long_name, String::new());
        malformed.insert("Valid".to_string(), String::new());
        malformed.insert("".to_string(), String::new());
        malformed.insert("\t".to_string(), String::new());
        malformed.insert(CLEAR_SENTINEL.to_string(), String::new());
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
