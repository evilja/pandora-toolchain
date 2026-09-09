use super::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use serenity::all::{ActionRowComponent, ButtonStyle, ComponentInteraction, CreateButton,
    CreateEmbed, CreateEmbedFooter, CreateInputText, CreateModal, InputTextStyle, ModalInteraction};
use pandora_toolchain::lib::sync::lock;
use pandora_toolchain::pnworker::messages::*;
use pandora_toolchain::pnworker::server_config::{server_meta_path, validate_preset_delivery, drive_only_from_meta, hls_from_meta, FansubSite};

struct Step {
    key: &'static str,
    fields: &'static [&'static str],
    upload: bool,
}

const STEPS: &[Step] = &[
    Step { key: "BASICS", fields: &["language", "announcement_channel"], upload: false },
    Step { key: "GITHUB", fields: &["github", "api_key"], upload: false },
    Step { key: "DELIVERY", fields: &["local_gdrive", "drive_only", "hls"], upload: false },
    Step { key: "ENCODE", fields: &["preset", "wrapstyle"], upload: false },
    Step { key: "CHANNEL", fields: &["channel_rename", "merge_release_only", "hls_name"], upload: false },
    Step { key: "FANSUB", fields: &["animecix_fansub", "openanime_fansub", "anizm_fansub"], upload: false },
    Step { key: "INTRO", fields: &["concat"], upload: true },
    Step { key: "OUTRO", fields: &["outro"], upload: true },
    Step { key: "WATERMARK", fields: &[], upload: true },
    Step { key: "LOGO", fields: &[], upload: true },
    Step { key: "PLACEMENT", fields: &["position", "margin", "opacity", "width", "period"], upload: false },
];

#[derive(Clone)]
struct Setup {
    id: u64,
    guild: u64,
    channel: u64,
    owner: u64,
    page: usize,
    token: String,
    touched: Instant,
    busy: bool,
}

fn sessions() -> &'static Mutex<HashMap<u64, Setup>> {
    static SESSIONS: OnceLock<Mutex<HashMap<u64, Setup>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn allowed(owner: u64, permissions: Option<Permissions>) -> bool {
    is_authorized("configure", owner)
        && (has_level_at_least(owner, 4)
            || permissions.is_some_and(|p| p.contains(Permissions::ADMINISTRATOR)))
}

fn step_message(setup: &Setup, note: Option<&str>) -> CreateInteractionResponseMessage {
    let lang = read_lang(Some(serenity::all::GuildId::new(setup.guild)));
    let step = &STEPS[setup.page];
    let mut body = get_message(&format!("CONFIG_{}_BODY", step.key), &lang);
    if let Some(command) = upload_command(step.key) {
        if !is_authorized(command, setup.owner) {
            body.push_str("\n\n");
            body.push_str(&get_message(CONFIG_MEDIA_FORBIDDEN, &lang));
        }
    }
    body.push_str("\n\n");
    body.push_str(&get_message(CONFIG_SKIP_HELP, &lang));
    if let Some(note) = note { body = format!("{note}\n\n{body}"); }
    let mut buttons = Vec::new();
    if !step.fields.is_empty() {
        buttons.push(CreateButton::new(format!("pnconfig:{}:{}:enter", setup.id, setup.page))
            .label(get_message(CONFIG_ENTER, &lang)).style(ButtonStyle::Primary));
    }
    buttons.push(CreateButton::new(format!("pnconfig:{}:{}:skip", setup.id, setup.page))
        .label(get_message(CONFIG_SKIP, &lang)).style(ButtonStyle::Secondary));
    buttons.push(CreateButton::new(format!("pnconfig:{}:{}:finish", setup.id, setup.page))
        .label(get_message(CONFIG_FINISH, &lang)).style(ButtonStyle::Secondary));
    CreateInteractionResponseMessage::new().content("").embed(CreateEmbed::new()
        .title(get_message(&format!("CONFIG_{}_TITLE", step.key), &lang))
        .description(body)
        .footer(CreateEmbedFooter::new(format!("{} / {}", setup.page + 1, STEPS.len()))))
        .components(vec![CreateActionRow::Buttons(buttons)])
}

fn completed(guild: u64) -> CreateInteractionResponseMessage {
    let lang = read_lang(Some(serenity::all::GuildId::new(guild)));
    let meta = ServerMetaFields::parse(&std::fs::read_to_string(server_meta_path(guild)).unwrap_or_default());
    let readiness = if meta.forgejo.is_empty() || meta.api_key.is_empty() { CONFIG_GITHUB_MISSING } else { CONFIG_INIT_READY };
    CreateInteractionResponseMessage::new().content(format!("{}\n\n{}",
        get_message(CONFIG_DONE, &lang), get_message(readiness, &lang)))
        .embeds(Vec::new()).components(Vec::new())
}

// Every button identifies its original session and page. Old messages cannot skip or overwrite a
// newer setup, and the busy flag keeps a double-click from submitting a step twice.
fn take_session(id: &str, owner: u64, guild: u64, channel: u64) -> Option<(Setup, String)> {
    let mut parts = id.split(':');
    if parts.next()? != "pnconfig" { return None; }
    let session_id = parts.next()?.parse::<u64>().ok()?;
    let page = parts.next()?.parse::<usize>().ok()?;
    let action = parts.next()?;
    if parts.next().is_some() || !matches!(action, "enter" | "skip" | "finish" | "save") { return None; }
    let mut all = lock(sessions());
    all.retain(|_, s| s.busy || s.touched.elapsed() < Duration::from_secs(600));
    let setup = all.get_mut(&guild)?;
    if setup.id != session_id || setup.owner != owner || setup.channel != channel || setup.page != page || setup.busy { return None; }
    setup.busy = true;
    setup.touched = Instant::now();
    Some((setup.clone(), action.to_string()))
}

fn release(setup: &Setup) {
    if let Some(current) = lock(sessions()).get_mut(&setup.guild).filter(|s| s.id == setup.id) {
        *current = setup.clone();
        current.busy = false;
        current.touched = Instant::now();
    }
}

fn discard(setup: &Setup) {
    let mut all = lock(sessions());
    if all.get(&setup.guild).is_some_and(|s| s.id == setup.id) { all.remove(&setup.guild); }
}

fn advance(setup: &mut Setup) -> CreateInteractionResponseMessage {
    setup.page += 1;
    if setup.page == STEPS.len() {
        lock(sessions()).remove(&setup.guild);
        completed(setup.guild)
    } else {
        release(setup);
        step_message(setup, None)
    }
}

pub async fn handle_configure(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let Some(guild) = command_server_id(ctx, command, "/configure").await else { return; };
    let lang = read_lang(command.guild_id);
    let setup = Setup { id: command.id.get(), guild, channel: command.channel_id.get(),
        owner: command.user.id.get(), page: 0, token: command.token.clone(), touched: Instant::now(), busy: false };
    let occupied = {
        let mut all = lock(sessions());
        all.retain(|_, s| s.busy || s.touched.elapsed() < Duration::from_secs(600));
        if all.contains_key(&guild) { true } else { all.insert(guild, setup.clone()); false }
    };
    if occupied {
        command_error(ctx, command, get_message(CONFIG_OCCUPIED, &lang)).await;
        return;
    }
    if !server_meta_path(guild).exists() && save_patch(guild, setup.id, &[]).is_err() {
        lock(sessions()).remove(&guild);
        command_error(ctx, command, get_message(CONFIG_SAVE_FAILED, &lang)).await;
        return;
    }
    if let Err(error) = command.create_response(ctx,
        CreateInteractionResponse::Message(step_message(&setup, None).ephemeral(true))).await {
        lock(sessions()).remove(&guild);
        report_interaction_failure("configure start", command, &error);
    }
}

pub async fn handle_configure_component(ctx: &Context, component: &ComponentInteraction) {
    let guild = component.guild_id.map(|g| g.get()).unwrap_or(0);
    let lang = read_lang(component.guild_id);
    let session = if allowed(component.user.id.get(), component.member.as_ref().and_then(|m| m.permissions)) {
        take_session(&component.data.custom_id, component.user.id.get(), guild, component.channel_id.get())
    } else { None };
    let Some((mut setup, action)) = session else {
        component.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new().content(get_message(CONFIG_EXPIRED, &lang)).ephemeral(true)
        )).await.ok();
        return;
    };
    let response = match action.as_str() {
        "enter" if !STEPS[setup.page].fields.is_empty() => {
            let step = &STEPS[setup.page];
            let rows = step.fields.iter().map(|field| CreateActionRow::InputText(
                CreateInputText::new(InputTextStyle::Short, *field, *field)
                    .required(false).max_length(1000)
            )).collect();
            release(&setup);
            CreateInteractionResponse::Modal(CreateModal::new(
                format!("pnconfig:{}:{}:save", setup.id, setup.page),
                get_message(&format!("CONFIG_{}_TITLE", step.key), &lang).chars().take(45).collect::<String>()
            ).components(rows))
        }
        "skip" => {
            setup.token = component.token.clone();
            CreateInteractionResponse::UpdateMessage(advance(&mut setup))
        }
        "finish" => {
            lock(sessions()).remove(&guild);
            CreateInteractionResponse::UpdateMessage(completed(guild))
        }
        _ => { release(&setup); CreateInteractionResponse::Acknowledge }
    };
    if let Err(error) = component.create_response(ctx, response).await {
        discard(&setup);
        eprintln!("configure navigation failed: {error}");
    }
}

pub async fn handle_configure_modal(ctx: &Context, modal: &ModalInteraction) {
    let guild = modal.guild_id.map(|g| g.get()).unwrap_or(0);
    let lang = read_lang(modal.guild_id);
    let session = if allowed(modal.user.id.get(), modal.member.as_ref().and_then(|m| m.permissions)) {
        take_session(&modal.data.custom_id, modal.user.id.get(), guild, modal.channel_id.get())
    } else { None };
    let Some((mut setup, action)) = session else {
        modal.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new().content(get_message(CONFIG_EXPIRED, &lang)).ephemeral(true)
        )).await.ok();
        return;
    };
    if action != "save" { release(&setup); return; }
    if modal.create_response(ctx, CreateInteractionResponse::Acknowledge).await.is_err() {
        release(&setup); return;
    }
    setup.token = modal.token.clone();
    let fields = STEPS[setup.page].fields;
    let values: Vec<(String, String)> = modal.data.components.iter().flat_map(|row| &row.components)
        .filter_map(|c| match c { ActionRowComponent::InputText(input) =>
            input.value.as_deref().map(str::trim).filter(|v| !v.is_empty())
                .filter(|_| fields.contains(&input.custom_id.as_str()))
                .map(|v| (input.custom_id.clone(), v.to_string())), _ => None }).collect();
    let result = apply_values(&setup, values).await;
    let response = match result {
        Ok(()) => advance(&mut setup),
        Err(id) => { release(&setup); step_message(&setup, Some(&get_message(id, &lang))) }
    };
    if !update_setup_message(ctx, &setup.token, response).await { discard(&setup); }
}

async fn update_setup_message(ctx: &Context, token: &str, response: CreateInteractionResponseMessage) -> bool {
    // These builders serialize the same content/embed/component fields. Use the public edit builder
    // so Discord replaces the old page instead of appending another setup message.
    let value = serde_json::to_value(response).unwrap_or_default();
    if let Err(error) = ctx.http.edit_original_interaction_response(token, &value, Vec::new()).await {
        eprintln!("configure page update failed: {error}");
        return false;
    }
    true
}

pub(super) fn github_org_url(value: &str) -> Result<String, &'static str> {
    let url = reqwest::Url::parse(value).map_err(|_| CONFIG_GITHUB_URL_INVALID)?;
    let org = url.path().trim_start_matches('/').trim_end_matches('/');
    if url.scheme() != "https" || url.host_str() != Some("github.com") || url.port().is_some()
        || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some()
        || org.is_empty() || !org.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
        return Err(CONFIG_GITHUB_URL_INVALID);
    }
    Ok(format!("https://github.com/{org}"))
}

fn validated(field: &str, value: &str, channel: u64) -> Result<String, &'static str> {
    if value.contains(['\r', '\n', '\0']) { return Err(CONFIG_INVALID); }
    let value = value.trim();
    match field {
        "language" => {
            let value = value.to_ascii_uppercase();
            if matches!(value.as_str(), "EN" | "TR" | "JP") { Ok(value) } else { Err(CONFIG_INVALID) }
        }
        "github" if value != "-" => github_org_url(value),
        "announcement_channel" => match value {
            "true" => Ok(channel.to_string()), "false" => Ok(String::new()), _ => Err(CONFIG_INVALID),
        },
        "local_gdrive" | "drive_only" | "hls" | "channel_rename" | "merge_release_only" => {
            if matches!(value, "true" | "false") { Ok(value.to_string()) } else { Err(CONFIG_INVALID) }
        }
        "wrapstyle" => match value {
            "dont_touch" | "-" => Ok(String::new()),
            "0" | "1" | "2" | "3" => Ok(value.to_string()), _ => Err(CONFIG_INVALID),
        },
        "preset" => {
            let name = value.to_ascii_lowercase();
            if pandora_toolchain::pnworker::server_effects::preset_from_name(&name, Concat::NONE).is_some() {
                Ok(name)
            } else { Err(CONFIG_INVALID) }
        }
        "hls_name" => {
            if value == "-" { return Ok(String::new()); }
            pandora_toolchain::lib::mpeg::hls::validate_name_template(value).map_err(|_| CONFIG_INVALID)
        }
        "concat" | "outro" if value != "-" => {
            let kind = if field == "concat" { ConcatKind::Intro } else { ConcatKind::Outro };
            if ConcatConfig::load_kind(kind).resolve(value).is_some() { Ok(value.to_string()) } else { Err(CONFIG_INVALID) }
        }
        _ => Ok(if value == "-" { String::new() } else { value.to_string() }),
    }
}

async fn apply_values(setup: &Setup, mut values: Vec<(String, String)>) -> Result<(), &'static str> {
    if values.is_empty() { return Ok(()); }
    if STEPS[setup.page].key == "PLACEMENT" {
        if !is_authorized("touchlogo", setup.owner) { return Err(CONFIG_MEDIA_FORBIDDEN); }
        return save_placement(setup.guild, &values);
    }
    for (field, value) in &mut values {
        *value = validated(field, value, setup.channel)?;
        if let Some(site) = FansubSite::from_option_name(field) {
            if !value.is_empty() {
                *value = resolve_fansub_selection(site, value).await.map_err(|_| CONFIG_INVALID)?.value;
            }
        }
    }
    save_patch(setup.guild, setup.id, &values)
}

// Re-read immediately before writing. Each step changes only its submitted fields, preserving
// skipped fields and settings changed through /edit while the reader was filling in the form.
fn save_patch(guild: u64, operation: u64, values: &[(String, String)]) -> Result<(), &'static str> {
    let path = server_meta_path(guild);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err(CONFIG_SAVE_FAILED),
    };
    let mut fields = ServerMetaFields::parse(&text);
    for (name, value) in values { fields.set(name, value.clone()).map_err(|_| CONFIG_INVALID)?; }
    let body = compose_server_meta(&fields);
    validate_preset_delivery(&fields.preset, drive_only_from_meta(&body), hls_from_meta(&body))
        .map_err(|_| CONFIG_DELIVERY_INVALID)?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|_| CONFIG_SAVE_FAILED)?;
    let temp = path.with_extension(format!("{operation}.tmp"));
    let result = (|| {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let mut file = options.open(&temp).map_err(|_| CONFIG_SAVE_FAILED)?;
        file.write_all(compose_server_meta(&fields).as_bytes()).map_err(|_| CONFIG_SAVE_FAILED)?;
        std::fs::rename(&temp, &path).map_err(|_| CONFIG_SAVE_FAILED)
    })();
    if result.is_err() { std::fs::remove_file(temp).ok(); }
    result
}

fn save_placement(guild: u64, values: &[(String, String)]) -> Result<(), &'static str> {
    use pandora_toolchain::lib::mpeg::logo::*;
    use pandora_toolchain::pnworker::server_effects::{load_server_logo, save_server_logo};
    let mut logo = load_server_logo(guild).ok_or(CONFIG_LOGO_MISSING)?;
    for (name, value) in values {
        match name.as_str() {
            "position" => logo.placement.position = LogoPosition::from_name(value).ok_or(CONFIG_INVALID)?,
            "margin" => logo.placement.margin = value.parse::<u32>().ok().filter(|v| *v <= MAX_LOGO_MARGIN).ok_or(CONFIG_INVALID)?,
            "opacity" => logo.placement.opacity = value.parse::<u8>().ok().filter(|v| (1..=100).contains(v)).ok_or(CONFIG_INVALID)?,
            "width" => logo.placement.width_percent = match value.parse::<u8>().map_err(|_| CONFIG_INVALID)? {
                0 => None, v if (MIN_LOGO_WIDTH_PERCENT..=MAX_LOGO_WIDTH_PERCENT).contains(&v) => Some(v), _ => return Err(CONFIG_INVALID),
            },
            "period" => logo.placement.period = if matches!(value.as_str(), "off" | "-") { None }
                else { Some(LogoPeriod::parse(value).map_err(|_| CONFIG_INVALID)?) },
            _ => return Err(CONFIG_INVALID),
        }
    }
    save_server_logo(guild, &logo).map_err(|_| CONFIG_SAVE_FAILED)
}

// Only attachments from the active setup's owner, in the same channel, are consumed. Text is never
// interpreted as a setting: tokens belong in the private modal, not in a public channel message.
pub async fn handle_configure_upload(ctx: &Context, message: &Message) -> bool {
    if message.author.bot || message.attachments.is_empty() { return false; }
    let Some(guild) = message.guild_id else { return false; };
    let setup = {
        let mut all = lock(sessions());
        all.retain(|_, s| s.busy || s.touched.elapsed() < Duration::from_secs(600));
        all.get_mut(&guild.get()).filter(|s| s.owner == message.author.id.get()
            && s.channel == message.channel_id.get() && !s.busy && STEPS[s.page].upload)
            .map(|s| { s.busy = true; s.clone() })
    };
    let Some(mut setup) = setup else { return false; };
    let permissions = match (guild.to_partial_guild(ctx).await, guild.member(ctx, message.author.id).await) {
        (Ok(g), Ok(m)) => Some(g.member_permissions(&m)), _ => None,
    };
    if !allowed(message.author.id.get(), permissions) {
        lock(sessions()).remove(&setup.guild);
        update_setup_message(ctx, &setup.token, CreateInteractionResponseMessage::new()
            .content(get_message(CONFIG_EXPIRED, &read_lang(message.guild_id)))
            .embeds(Vec::new()).components(Vec::new())).await;
        return true;
    }
    let lang = read_lang(message.guild_id);
    update_setup_message(ctx, &setup.token, CreateInteractionResponseMessage::new()
        .content(get_message(CONFIG_PROCESSING, &lang)).embeds(Vec::new()).components(Vec::new())).await;
    let result = if message.attachments.len() != 1 { Err(CONFIG_MEDIA_INVALID) }
        else { install_media(&setup, &message.attachments[0], message.id.get()).await };
    let response = match result {
        Ok(()) => advance(&mut setup),
        Err(id) => { release(&setup); step_message(&setup, Some(&get_message(id, &lang))) }
    };
    if !update_setup_message(ctx, &setup.token, response).await { discard(&setup); }
    true
}

fn upload_command(step: &str) -> Option<&'static str> {
    match step {
        "INTRO" => Some("touchintro"), "OUTRO" => Some("touchoutro"),
        "WATERMARK" => Some("touchwatermark"), "LOGO" => Some("touchlogo"), _ => None,
    }
}

async fn install_media(setup: &Setup, attachment: &serenity::all::Attachment, operation: u64) -> Result<(), &'static str> {
    let step = STEPS[setup.page].key;
    if !upload_command(step).is_some_and(|cmd| is_authorized(cmd, setup.owner)) {
        return Err(CONFIG_MEDIA_FORBIDDEN);
    }
    let max = if matches!(step, "WATERMARK" | "LOGO") { 4 * 1024 * 1024 } else { 100 * 1024 * 1024 };
    if attachment.size as usize > max { return Err(CONFIG_MEDIA_INVALID); }
    let bytes = attachment.download().await.map_err(|_| CONFIG_MEDIA_INVALID)?;
    if bytes.len() > max { return Err(CONFIG_MEDIA_INVALID); }
    match step {
        "INTRO" | "OUTRO" => {
            let kind = if step == "INTRO" { ConcatKind::Intro } else { ConcatKind::Outro };
            let name = format!("setup_{}_{}", setup.guild, operation);
            super::addintro::install_concat_upload(setup.guild, operation, kind, &name, &bytes, |_, _| async {}).await
                .map_err(|_| CONFIG_MEDIA_INVALID)?;
            let field = if step == "INTRO" { "concat" } else { "outro" };
            save_patch(setup.guild, operation, &[(field.into(), name)])
        }
        "WATERMARK" => super::watermark::save_watermark_upload(setup.guild, operation, &attachment.filename, bytes)
            .await.map(|_| ()).map_err(|_| CONFIG_MEDIA_INVALID),
        "LOGO" => {
            use pandora_toolchain::lib::mpeg::logo::{detect_logo_format, ServerLogo};
            use pandora_toolchain::pnworker::server_effects::{load_server_logo, save_server_logo};
            let extension = detect_logo_format(&bytes).ok_or(CONFIG_MEDIA_INVALID)?.to_string();
            let placement = load_server_logo(setup.guild).map(|l| l.placement).unwrap_or_default();
            save_server_logo(setup.guild, &ServerLogo { bytes, extension, placement }).map_err(|_| CONFIG_SAVE_FAILED)
        }
        _ => Err(CONFIG_INVALID),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_sessions_reject_other_users_old_pages_duplicate_submits_and_expiry() {
        let guild = u64::MAX - 41;
        let setup = Setup { id: 123, guild, owner: 42, channel: 7, page: 0,
            token: String::new(), touched: Instant::now(), busy: false };
        lock(sessions()).insert(guild, setup.clone());
        for (id, owner, server, channel) in [
            ("pnconfig:123:0:skip", 43, guild, 7),
            ("pnconfig:123:0:skip", 42, guild, 8),
            ("pnconfig:123:0:skip", 42, guild - 1, 7),
            ("pnconfig:124:0:skip", 42, guild, 7),
            ("pnconfig:123:1:skip", 42, guild, 7),
            ("pnconfig:123:0:skip:extra", 42, guild, 7),
        ] { assert!(take_session(id, owner, server, channel).is_none()); }
        let (mut taken, _) = take_session("pnconfig:123:0:skip", 42, guild, 7).unwrap();
        assert!(take_session("pnconfig:123:0:skip", 42, guild, 7).is_none());
        taken.page = 1;
        release(&taken);
        assert!(take_session("pnconfig:123:0:save", 42, guild, 7).is_none());
        assert!(take_session("pnconfig:123:1:save", 42, guild, 7).is_some());
        release(&taken);
        lock(sessions()).get_mut(&guild).unwrap().touched = Instant::now() - Duration::from_secs(601);
        assert!(take_session("pnconfig:123:1:skip", 42, guild, 7).is_none());
        assert!(!lock(sessions()).contains_key(&guild));
    }

    #[test]
    fn setup_rejects_invalid_values_without_echoing_tokens() {
        for value in ["https://github.com", "https://github.com/team/repo",
                      "https://secret@github.com/team", "http://github.com/team",
                      "https://github.com.evil.example/team", "https://github.com/team?token=secret"] {
            assert_eq!(validated("github", value, 7), Err(CONFIG_GITHUB_URL_INVALID));
        }
        assert_eq!(validated("github", "https://github.com/team/", 7).unwrap(), "https://github.com/team");
        assert_eq!(validated("api_key", "secret\ninjected-line", 7), Err(CONFIG_INVALID));
        assert_eq!(validated("language", "tr", 7).unwrap(), "TR");
        assert_eq!(validated("hls", "perhaps", 7), Err(CONFIG_INVALID));
        assert_eq!(validated("wrapstyle", "4", 7), Err(CONFIG_INVALID));
        assert_eq!(validated("wrapstyle", "dont_touch", 7).unwrap(), "");
        assert_eq!(validated("announcement_channel", "true", 7).unwrap(), "7");
    }

    #[test]
    fn partial_setup_edits_preserve_other_settings_and_reserved_lines() {
        let original = "TR\nhttps://git.example.com/team\n123\nprivate-token\nold4\nold5\nold6\nold7\n2\nfalse\nold10\nstandard\nintro\nfan13\ntrue\nfan15\nfan16\nfalse\n%uuid%\noutro\ntrue\nfalse\n";
        let mut fields = ServerMetaFields::parse(original);
        assert_eq!(compose_server_meta(&fields), original);
        fields.set("language", "JP".into()).unwrap();
        let expected = original.replacen("TR\n", "JP\n", 1);
        assert_eq!(compose_server_meta(&fields), expected);
        fields.set("github", "https://github.com/team".into()).unwrap();
        fields.set("api_key", String::new()).unwrap();
        assert_eq!(fields.forgejo, "https://github.com/team");
        assert_eq!(fields.outro, "outro");
        assert_eq!(fields.channel_rename, "false");
    }

    #[test]
    fn every_setup_step_has_translations_and_fits_a_discord_form() {
        for locale in [include_str!("../../pnworker/locales/en.toml"),
                       include_str!("../../pnworker/locales/tr.toml"),
                       include_str!("../../pnworker/locales/jp.toml")] {
            let entries: toml::Value = toml::from_str(locale).unwrap();
            for step in STEPS {
                assert!(step.fields.len() <= 5);
                for suffix in ["TITLE", "BODY"] {
                    let entry = &entries[format!("CONFIG_{}_{suffix}", step.key)];
                    let text = entry["text"].as_str().unwrap();
                    assert!(!text.is_empty());
                    assert!(text.encode_utf16().count() < if suffix == "TITLE" { 45 } else { 3000 });
                }
            }
        }
    }
}
