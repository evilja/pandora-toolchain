use super::*;

use pandora_toolchain::lib::http::mal::prequel_episode_total;
use pandora_toolchain::lib::source_doc::compose as compose_source;
use pandora_toolchain::lib::watch::{self, FeedItem, PendingRelease, ReleaseNumber, WatchConfig};
use serenity::all::{
    ButtonStyle, ChannelId, Colour, CommandDataOption, CommandDataOptionValue, ComponentInteraction,
    CreateActionRow, CreateButton, CreateInteractionResponseFollowup, GuildId,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const WATCH_COMPONENT_PREFIX: &str = "pnwatch";
const POLL_INTERVAL: Duration = Duration::from_secs(600);
// Rows of the numbering preview. Enough to see the pattern, few enough to read at a glance.
const PREVIEW_ROWS: usize = 8;
// Discord's own limit on the options of one select menu.
const SELECT_LIMIT: usize = 25;
// The commands whose `episode` may be typed as a release number (see `translate_release_episode`).
// The publish commands are left out on purpose: they speak each site's own numbering.
const RELEASE_EPISODE_COMMANDS: &[&str] = &["smartcode", "merge", "release", "source", "get", "job"];

// One lock for every watch. A check reads the file, talks to the feed and the repo, and writes the
// file back; a button pressed meanwhile must not have its change overwritten by that write, and a
// "Check now" racing the poller must not announce the same episode twice. Checks are rare and
// short, so one lock for all of them costs nothing.
fn watch_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// `/watch` — with `feed` it sets a watch up, with `stop` it ends one, and with neither it shows
// the one this channel has.
pub async fn handle_watch(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let Some(server_id) = command_server_id(ctx, command, "/watch").await else {
        return;
    };
    let channel_id = command.channel_id.get();
    let Some((meta, _owner_repo, _repo_url)) = attached_repo(ctx, command, server_id, None).await else {
        return;
    };
    let lang = command_language(command);

    if option_bool(command, "stop") == Some(true) {
        let _guard = watch_lock().lock().await;
        let had_one = watch::load(server_id, channel_id).is_some();
        watch::remove(server_id, channel_id).await;
        let text = get_message(if had_one { WATCH_STOPPED } else { WATCH_NONE }, &lang);
        reply(ctx, command, text, !had_one).await;
        return;
    }

    match option_trimmed(command, "feed") {
        Some(feed) => setup_watch(ctx, command, server_id, channel_id, &meta, &feed, &lang).await,
        None => show_watch(ctx, command, server_id, channel_id, &meta, &lang).await,
    }
}

async fn reply(ctx: &Context, command: &serenity::all::CommandInteraction, text: String, ephemeral: bool) {
    if let Err(error) = command
        .create_response(
            ctx,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new().content(text).ephemeral(ephemeral),
            ),
        )
        .await
    {
        report_interaction_failure("watch reply", command, &error);
    }
}

// Reads the feed, guesses the numbering, and asks for it to be confirmed. The watch is saved
// unconfirmed right away, so the question survives a restart, but it does nothing until then.
async fn setup_watch(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    server_id: u64,
    channel_id: u64,
    meta: &ChannelMeta,
    feed: &str,
    lang: &str,
) {
    let feed_url = match watch::feed_url(feed) {
        Ok(url) => url,
        Err(e) => {
            command_error(ctx, command, format_message(WATCH_FEED_FAILED, lang, &[e])).await;
            return;
        }
    };
    let Some(mut response) = working_response(ctx, command, "...").await else {
        return;
    };
    let items = match watch::fetch_feed(&feed_url).await {
        Ok(items) => items,
        Err(e) => {
            let _ = response
                .edit(ctx, EditMessage::new().content(format_message(WATCH_FEED_FAILED, lang, &[e])))
                .await;
            return;
        }
    };

    let releases = numbered(&items);
    let numbers: Vec<ReleaseNumber> = releases.iter().map(|(_, release)| *release).collect();
    let episode_count = meta.episode_count.unwrap_or(0);
    let season = watch::feed_season(&numbers, meta.season as u32);
    let bare = bare_numbers(&numbers);
    // Only asked when a bare number is involved: titles that all name their season never go
    // through the offset, and the chain walk is a dozen requests to JIKAN.
    let seasonal_only = bare.is_empty() && !numbers.is_empty();
    let prequel_total = match (seasonal_only, meta.mal_id) {
        (false, Some(mal_id)) => prequel_episode_total(mal_id).await,
        _ => None,
    };
    let config = WatchConfig {
        mal_id: meta.mal_id.unwrap_or(0),
        feed: feed.to_string(),
        feed_url,
        offset: watch::guess_offset(&bare, episode_count, prequel_total),
        season,
        confirmed: false,
        created_by: command.user.id.get(),
        setup_message: Some(response.id.get()),
        ..Default::default()
    };
    {
        let _guard = watch_lock().lock().await;
        if let Err(e) = watch::save(server_id, channel_id, &config).await {
            let _ = response
                .edit(ctx, EditMessage::new().content(format_message(WATCH_FEED_FAILED, lang, &[e])))
                .await;
            return;
        }
    }
    let (embed, components) = watch_view(channel_id, &config, episode_count, Some(&items), prequel_total, lang);
    let _ = response
        .edit(ctx, EditMessage::new().content("").embed(embed).components(components))
        .await;
}

async fn show_watch(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    server_id: u64,
    channel_id: u64,
    meta: &ChannelMeta,
    lang: &str,
) {
    let Some(config) = watch::load(server_id, channel_id) else {
        reply(ctx, command, get_message(WATCH_NONE, lang), true).await;
        return;
    };
    if !config.applies_to(meta.mal_id) {
        reply(ctx, command, get_message(WATCH_REATTACHED, lang), true).await;
        return;
    }
    let Some(mut response) = working_response(ctx, command, "...").await else {
        return;
    };
    let fetched = watch::fetch_feed(&config.feed_url).await;
    let episode_count = meta.episode_count.unwrap_or(0);
    let (mut embed, components) = match &fetched {
        Ok(items) => watch_view(channel_id, &config, episode_count, Some(items), None, lang),
        Err(_) => watch_view(channel_id, &config, episode_count, None, None, lang),
    };
    if let Err(e) = fetched {
        embed = embed.field(get_message(WATCH_FIELD_ERROR, lang), clip(&e, 1000), false);
    }
    let _ = response
        .edit(ctx, EditMessage::new().content("").embed(embed).components(components))
        .await;
}

// Feed items that are one episode each, with the number their title carries.
fn numbered(items: &[FeedItem]) -> Vec<(&FeedItem, ReleaseNumber)> {
    items
        .iter()
        .filter_map(|item| watch::release_number(&item.title).map(|release| (item, release)))
        .collect()
}

fn bare_numbers(numbers: &[ReleaseNumber]) -> Vec<u32> {
    numbers
        .iter()
        .filter(|release| release.season.is_none())
        .map(|release| release.number)
        .collect()
}

// A title as it is shown inside backticks: no backticks of its own, and short enough that a
// preview of eight rows stays readable.
fn clip(text: &str, limit: usize) -> String {
    let text = text.replace('`', "'");
    if text.chars().count() <= limit {
        return text;
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn numbering_line(config: &WatchConfig, numbers: &[ReleaseNumber], lang: &str) -> String {
    if !numbers.is_empty() && numbers.iter().all(|release| release.season.is_some()) {
        format_message(WATCH_NUMBERING_SEASON, lang, &[config.season.to_string()])
    } else if config.offset > 0 {
        format_message(WATCH_NUMBERING_OFFSET, lang, &[(config.offset + 1).to_string()])
    } else {
        get_message(WATCH_NUMBERING_SAME, lang)
    }
}

// The watch as one message: the feed, what its releases map to, and the buttons that fit the
// state it is in. `items` is `None` when the feed could not be read this time; `prequel_total` is
// only known during setup, where it offers the two readings of an empty feed.
fn watch_view(
    channel_id: u64,
    config: &WatchConfig,
    episode_count: u32,
    items: Option<&[FeedItem]>,
    prequel_total: Option<u32>,
    lang: &str,
) -> (CreateEmbed, Vec<CreateActionRow>) {
    let releases = items.map(numbered).unwrap_or_default();
    let numbers: Vec<ReleaseNumber> = releases.iter().map(|(_, release)| *release).collect();

    let mut description = format_message(WATCH_SETUP_BODY, lang, &[format!("`{}`", clip(&config.feed, 200))]);
    description.push_str("\n\n");
    description.push_str(&numbering_line(config, &numbers, lang));
    if items.is_some() {
        description.push_str("\n\n");
        if releases.is_empty() {
            description.push_str(&get_message(WATCH_NO_RELEASES, lang));
        } else {
            // Oldest first, the order the episodes run in.
            let mut shown: Vec<&(&FeedItem, ReleaseNumber)> = releases.iter().take(PREVIEW_ROWS).collect();
            shown.reverse();
            let rows: Vec<String> = shown
                .into_iter()
                .map(|(item, release)| {
                    let title = clip(&item.title, 80);
                    match watch::map_release(release, config.season, episode_count, config.offset) {
                        Some(episode) => format_message(WATCH_ROW, lang, &[title, episode.to_string()]),
                        None => format_message(WATCH_ROW_OUTSIDE, lang, &[title]),
                    }
                })
                .collect();
            description.push_str(&rows.join("\n"));
        }
    }
    description.push_str("\n\n");
    description.push_str(&get_message(if config.confirmed { WATCH_CONFIRMED } else { WATCH_NOT_CONFIRMED }, lang));

    let mut embed = CreateEmbed::new()
        .title(get_message(WATCH_TITLE, lang))
        .colour(if config.confirmed { Colour::DARK_GREEN } else { Colour::BLUE })
        .description(description)
        .timestamp(serenity::model::Timestamp::now());
    if config.confirmed {
        let never = get_message(WATCH_NEVER, lang);
        embed = embed
            .field(
                get_message(WATCH_FIELD_LAST_RELEASE, lang),
                config.last_release.as_deref().map(|title| format!("`{}`", clip(title, 200))).unwrap_or_else(|| never.clone()),
                false,
            )
            .field(
                get_message(WATCH_FIELD_LAST_CHECK, lang),
                config.last_checked.map(|at| format!("<t:{}:R>", at)).unwrap_or(never),
                true,
            );
        if !config.pending.is_empty() {
            embed = embed.field(get_message(WATCH_FIELD_WAITING, lang), config.pending.len().to_string(), true);
        }
        if let Some(error) = &config.last_error {
            embed = embed.field(get_message(WATCH_FIELD_ERROR, lang), clip(error, 1000), false);
        }
    }

    let id = |action: &str| component_id(channel_id, config, action);
    let mut rows = Vec::new();
    if config.confirmed {
        rows.push(CreateActionRow::Buttons(vec![
            CreateButton::new(id("check")).label(get_message(WATCH_BUTTON_CHECK, lang)).style(ButtonStyle::Primary),
            CreateButton::new(id("stop")).label(get_message(WATCH_BUTTON_STOP, lang)).style(ButtonStyle::Danger),
        ]));
    } else {
        rows.push(CreateActionRow::Buttons(vec![
            CreateButton::new(id("ok")).label(get_message(WATCH_BUTTON_OK, lang)).style(ButtonStyle::Success),
            CreateButton::new(id("cancel")).label(get_message(WATCH_BUTTON_CANCEL, lang)).style(ButtonStyle::Danger),
        ]));
    }

    // Picking episode 1 out of the feed's own titles is the whole of changing the numbering. Titles
    // that name their season never go through it, so they get no picker.
    let mut firsts: Vec<(u32, String)> = Vec::new();
    for (item, release) in &releases {
        if release.season.is_none() && !firsts.iter().any(|(number, _)| *number == release.number) {
            firsts.push((release.number, item.title.clone()));
        }
    }
    firsts.sort_by_key(|(number, _)| *number);
    firsts.truncate(SELECT_LIMIT);
    if !firsts.is_empty() {
        let current = config.offset + 1;
        let options = firsts
            .into_iter()
            .map(|(number, title)| {
                CreateSelectMenuOption::new(clip(&title, 100), number.to_string())
                    .default_selection(number == current)
            })
            .collect();
        rows.push(CreateActionRow::SelectMenu(
            CreateSelectMenu::new(id("first"), CreateSelectMenuKind::String { options })
                .placeholder(get_message(WATCH_PICK_FIRST, lang)),
        ));
    } else if !config.confirmed && releases.is_empty() {
        // An empty feed has nothing to pick from, but a sequel still has exactly two readings.
        if let Some(prequel) = prequel_total.filter(|total| *total > 0) {
            rows.push(CreateActionRow::Buttons(vec![
                CreateButton::new(id(&format!("offset:{}", prequel)))
                    .label(format_message(WATCH_BUTTON_CONTINUES, lang, &[(prequel + 1).to_string()]))
                    .style(ButtonStyle::Secondary),
                CreateButton::new(id("offset:0"))
                    .label(get_message(WATCH_BUTTON_RESTARTS, lang))
                    .style(ButtonStyle::Secondary),
            ]));
        }
    }
    (embed, rows)
}

// `pnwatch:<channel>:<revision>:<action>[:<arg>...]`. The revision is the setup message the watch
// was created from: a watch set up again gets a new one, so buttons left on an older message
// cannot act on the newer watch.
fn component_id(channel_id: u64, config: &WatchConfig, action: &str) -> String {
    format!(
        "{}:{}:{}:{}",
        WATCH_COMPONENT_PREFIX,
        channel_id,
        config.setup_message.unwrap_or(0),
        action
    )
}

#[derive(Debug, PartialEq, Eq)]
enum WatchAction {
    Confirm,
    Cancel,
    Check,
    Stop,
    First,
    Offset(u32),
    Assign(String),
    Use(String, u32),
    Ignore(String),
}

fn parse_component_id(id: &str) -> Option<(u64, u64, WatchAction)> {
    let mut parts = id.split(':');
    if parts.next()? != WATCH_COMPONENT_PREFIX {
        return None;
    }
    let channel_id = parts.next()?.parse().ok()?;
    let revision = parts.next()?.parse().ok()?;
    let action = match parts.next()? {
        "ok" => WatchAction::Confirm,
        "cancel" => WatchAction::Cancel,
        "check" => WatchAction::Check,
        "stop" => WatchAction::Stop,
        "first" => WatchAction::First,
        "offset" => WatchAction::Offset(parts.next()?.parse().ok()?),
        "assign" => WatchAction::Assign(parts.next()?.to_string()),
        "use" => WatchAction::Use(parts.next()?.to_string(), parts.next()?.parse().ok()?),
        "ignore" => WatchAction::Ignore(parts.next()?.to_string()),
        _ => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((channel_id, revision, action))
}

async fn component_notice(ctx: &Context, component: &ComponentInteraction, text: String) {
    component
        .create_response(
            ctx,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new().content(text).ephemeral(true),
            ),
        )
        .await
        .ok();
}

async fn component_replace(ctx: &Context, component: &ComponentInteraction, text: String) {
    component
        .create_response(
            ctx,
            CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .content(text)
                    .embeds(vec![])
                    .components(vec![]),
            ),
        )
        .await
        .ok();
}

fn selected_value(component: &ComponentInteraction) -> Option<&str> {
    match &component.data.kind {
        ComponentInteractionDataKind::StringSelect { values } => values.first().map(String::as_str),
        _ => None,
    }
}

pub async fn handle_watch_component(ctx: &Context, component: &ComponentInteraction) {
    let lang = read_lang(component.guild_id);
    let (Some(guild), Some((channel_id, revision, action))) =
        (component.guild_id, parse_component_id(&component.data.custom_id))
    else {
        let _ = component.create_response(ctx, CreateInteractionResponse::Acknowledge).await;
        return;
    };
    let server_id = guild.get();
    // Anyone who may run `/watch` may answer its questions; the messages the poller posts have no
    // author to restrict them to.
    if !is_authorized("watch", component.user.id.get()) {
        component_notice(ctx, component, get_message(WATCH_NOT_ALLOWED, &lang)).await;
        return;
    }

    let guard = watch_lock().lock().await;
    let Some(mut config) = watch::load(server_id, channel_id)
        .filter(|config| config.setup_message.unwrap_or(0) == revision)
    else {
        drop(guard);
        component_notice(ctx, component, get_message(WATCH_STALE, &lang)).await;
        return;
    };
    let meta = read_channel_meta(server_id, channel_id);
    if !config.applies_to(meta.mal_id) {
        drop(guard);
        component_notice(ctx, component, get_message(WATCH_REATTACHED, &lang)).await;
        return;
    }

    match action {
        WatchAction::Cancel => {
            if config.confirmed {
                drop(guard);
                let _ = component.create_response(ctx, CreateInteractionResponse::Acknowledge).await;
                return;
            }
            watch::remove(server_id, channel_id).await;
            drop(guard);
            component_replace(ctx, component, get_message(WATCH_CANCELLED, &lang)).await;
        }
        WatchAction::Stop => {
            watch::remove(server_id, channel_id).await;
            drop(guard);
            component_replace(ctx, component, get_message(WATCH_STOPPED, &lang)).await;
        }
        WatchAction::Confirm | WatchAction::First | WatchAction::Offset(_) => {
            let offset = match &action {
                WatchAction::First => selected_value(component)
                    .and_then(|value| value.parse::<u32>().ok())
                    .map(|first| first.saturating_sub(1)),
                WatchAction::Offset(offset) => Some(*offset),
                _ => None,
            };
            if let Some(offset) = offset.filter(|offset| *offset != config.offset) {
                config.offset = offset;
                // Questions asked under the old numbering are withdrawn and their releases looked at
                // again: under the new one most of them are no longer questions at all.
                for pending in std::mem::take(&mut config.pending) {
                    config.seen.retain(|key| key != &pending.key);
                }
            }
            config.confirmed = true;
            if let Err(e) = watch::save(server_id, channel_id, &config).await {
                drop(guard);
                component_notice(ctx, component, format_message(WATCH_WRITE_FAILED, &lang, &[e])).await;
                return;
            }
            drop(guard);
            let (embed, rows) = watch_view(channel_id, &config, meta.episode_count.unwrap_or(0), None, None, &lang);
            component
                .create_response(
                    ctx,
                    CreateInteractionResponse::UpdateMessage(
                        CreateInteractionResponseMessage::new().content("").embed(embed).components(rows),
                    ),
                )
                .await
                .ok();
            // Confirming is also the first check, which fills in the episodes already out.
            let ctx = ctx.clone();
            tokio::spawn(async move {
                if let Err(e) = check_watch(&ctx, server_id, channel_id).await {
                    let lang = read_lang(Some(GuildId::new(server_id)));
                    post(&ctx, channel_id, format_message(WATCH_FEED_FAILED, &lang, &[e]), vec![]).await;
                }
            });
        }
        WatchAction::Check => {
            drop(guard);
            component
                .create_response(
                    ctx,
                    CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true)),
                )
                .await
                .ok();
            let text = match check_watch(ctx, server_id, channel_id).await {
                Ok(written) => format_message(WATCH_CHECK_DONE, &lang, &[written.to_string()]),
                Err(e) => format_message(WATCH_FEED_FAILED, &lang, &[e]),
            };
            component
                .edit_response(ctx, EditInteractionResponse::new().content(text))
                .await
                .ok();
        }
        WatchAction::Ignore(key) => {
            let taken = config.take_pending(&key);
            let _ = watch::save(server_id, channel_id, &config).await;
            drop(guard);
            match taken {
                Some(release) => {
                    component_replace(ctx, component, format_message(WATCH_IGNORED, &lang, &[clip(&release.title, 200)])).await
                }
                None => component_notice(ctx, component, get_message(WATCH_STALE, &lang)).await,
            }
        }
        WatchAction::Assign(ref key) | WatchAction::Use(ref key, _) => {
            let episode = match &action {
                WatchAction::Use(_, episode) => Some(*episode),
                _ => selected_value(component).and_then(|value| value.parse::<u32>().ok()),
            };
            let Some(episode) = episode.filter(|episode| (1..=meta.episode_count.unwrap_or(0)).contains(episode)) else {
                drop(guard);
                let _ = component.create_response(ctx, CreateInteractionResponse::Acknowledge).await;
                return;
            };
            let Some(release) = config.pending.iter().find(|pending| &pending.key == key).cloned() else {
                drop(guard);
                component_notice(ctx, component, get_message(WATCH_STALE, &lang)).await;
                return;
            };
            // The person chose this episode for this release, so an existing source is replaced.
            let written = match RepoTarget::open(server_id, &meta).await {
                Ok(repo) => repo.write_source(server_id, channel_id, episode, &release.link).await,
                Err(e) => Err(e),
            };
            if let Err(e) = written {
                drop(guard);
                component_notice(ctx, component, format_message(WATCH_WRITE_FAILED, &lang, &[e])).await;
                return;
            }
            config.take_pending(key);
            config.last_release = Some(release.title.clone());
            let _ = watch::save(server_id, channel_id, &config).await;
            drop(guard);
            component_replace(
                ctx,
                component,
                format_message(WATCH_SOURCE_SET_SAME, &lang, &[episode.to_string(), clip(&release.title, 200)]),
            )
            .await;
        }
    }
}

// The attached repo, opened with the server's own credentials rather than a command's.
struct RepoTarget {
    fg: Forgejo,
    owner_repo: String,
}

impl RepoTarget {
    async fn open(server_id: u64, meta: &ChannelMeta) -> Result<Self, String> {
        let repo_url = meta
            .repo_url
            .clone()
            .filter(|url| !url.is_empty())
            .ok_or_else(|| "this channel has no repo URL configured".to_string())?;
        let (owner, repo) = parse_repo_url(&repo_url)?;
        let (_lang, forgejo_base, api_key) = read_server_meta(server_id).await?;
        if forgejo_base.is_empty() {
            return Err("the server has no repository organization configured".to_string());
        }
        Ok(Self {
            fg: Forgejo::new(forgejo_base, api_key)?,
            owner_repo: format!("{}/{}", owner, repo),
        })
    }

    async fn has_source(&self, episode: u32) -> Result<bool, String> {
        let path = format!("{}/SOURCE.md", pad2(episode));
        Ok(self.fg.get_file_content(&self.owner_repo, &path).await?.is_some())
    }

    // The same `SOURCE.md` `/source` writes for a single-episode link. A release that turns out to
    // be a pack is asked about by `/smartcode do`, exactly as a pack link without a recorded file.
    async fn write_source(&self, server_id: u64, channel_id: u64, episode: u32, link: &str) -> Result<(), String> {
        let path = format!("{}/SOURCE.md", pad2(episode));
        let content = compose_source(&display_source_link(link), None);
        self.fg
            .upsert_file(&self.owner_repo, &path, &base64_encode(&content), "Set source link (release watch)")
            .await?;
        remove_gitkeep_for_path(&self.fg, &self.owner_repo, &path).await;
        pandora_toolchain::lib::git::record_attachment_sync(server_id, channel_id).await;
        Ok(())
    }
}

async fn post(ctx: &Context, channel_id: u64, text: String, components: Vec<CreateActionRow>) {
    let message = CreateMessage::new().content(text).components(components);
    if let Err(error) = ChannelId::new(channel_id).send_message(ctx, message).await {
        report_send_failure("release watch", channel_id, &error);
    }
}

// Episodes to offer for a release that did not fit: all of them when they fit in one menu, and
// otherwise the window around where the release would have landed.
fn episode_choices(number: u32, offset: u32, episode_count: u32, lang: &str) -> Vec<CreateSelectMenuOption> {
    let limit = SELECT_LIMIT as u32;
    let (first, last) = if episode_count <= limit {
        (1, episode_count)
    } else {
        let guess = number.saturating_sub(offset).clamp(1, episode_count);
        let first = guess.saturating_sub(limit / 2).clamp(1, episode_count - limit + 1);
        (first, first + limit - 1)
    };
    (first..=last)
        .map(|episode| {
            CreateSelectMenuOption::new(format_message(WATCH_EPISODE_LABEL, lang, &[episode.to_string()]), episode.to_string())
        })
        .collect()
}

// Reads the feed once and handles every item it has not handled before, oldest first. Returns how
// many sources it wrote. An item is only marked handled once it has been dealt with — a repo that
// could not be reached leaves it for the next check rather than losing it.
async fn check_watch(ctx: &Context, server_id: u64, channel_id: u64) -> Result<usize, String> {
    let _guard = watch_lock().lock().await;
    let Some(mut config) = watch::load(server_id, channel_id) else {
        return Ok(0);
    };
    let meta = read_channel_meta(server_id, channel_id);
    // Unconfirmed, or set up for an anime this channel is no longer attached to: nothing to do, and
    // nothing worth a log line every ten minutes either.
    if !config.confirmed || !config.applies_to(meta.mal_id) {
        return Ok(0);
    }
    let lang = read_lang(Some(GuildId::new(server_id)));
    config.last_checked = Some(now_secs());

    let items = match watch::fetch_feed(&config.feed_url).await {
        Ok(items) => items,
        Err(e) => {
            config.last_error = Some(e.clone());
            let _ = watch::save(server_id, channel_id, &config).await;
            return Err(e);
        }
    };
    let repo = match RepoTarget::open(server_id, &meta).await {
        Ok(repo) => repo,
        Err(e) => {
            config.last_error = Some(e.clone());
            let _ = watch::save(server_id, channel_id, &config).await;
            return Err(e);
        }
    };

    let episode_count = meta.episode_count.unwrap_or(0);
    let mut written = 0;
    let mut error = None;
    for item in items.iter().rev() {
        if config.has_seen(&item.key) {
            continue;
        }
        // Batches, movies and ranges are not episodes, and not questions either.
        let Some(release) = watch::release_number(&item.title) else {
            config.mark_seen(&item.key);
            continue;
        };
        let title = clip(&item.title, 200);
        let Some(episode) = watch::map_release(&release, config.season, episode_count, config.offset) else {
            let pending = PendingRelease {
                key: item.key.clone(),
                title: item.title.clone(),
                link: item.link.clone(),
                number: release.number,
            };
            let rows = vec![
                CreateActionRow::SelectMenu(
                    CreateSelectMenu::new(
                        component_id(channel_id, &config, &format!("assign:{}", item.key)),
                        CreateSelectMenuKind::String {
                            options: episode_choices(release.number, config.offset, episode_count, &lang),
                        },
                    )
                    .placeholder(get_message(WATCH_PICK_EPISODE, &lang)),
                ),
                CreateActionRow::Buttons(vec![CreateButton::new(component_id(
                    channel_id,
                    &config,
                    &format!("ignore:{}", item.key),
                ))
                .label(get_message(WATCH_BUTTON_IGNORE, &lang))
                .style(ButtonStyle::Secondary)]),
            ];
            config.add_pending(pending);
            post(ctx, channel_id, format_message(WATCH_OUTSIDE, &lang, &[title, episode_count.to_string()]), rows).await;
            config.mark_seen(&item.key);
            continue;
        };
        match repo.has_source(episode).await {
            Err(e) => {
                error = Some(e);
                break;
            }
            // Another group's release, another resolution, or a source someone set by hand: the
            // episode is covered. Only a corrected re-release is worth a question.
            Ok(true) => {
                if release.version > 1 {
                    config.add_pending(PendingRelease {
                        key: item.key.clone(),
                        title: item.title.clone(),
                        link: item.link.clone(),
                        number: release.number,
                    });
                    let rows = vec![CreateActionRow::Buttons(vec![
                        CreateButton::new(component_id(channel_id, &config, &format!("use:{}:{}", item.key, episode)))
                            .label(get_message(WATCH_BUTTON_USE, &lang))
                            .style(ButtonStyle::Primary),
                        CreateButton::new(component_id(channel_id, &config, &format!("ignore:{}", item.key)))
                            .label(get_message(WATCH_BUTTON_IGNORE, &lang))
                            .style(ButtonStyle::Secondary),
                    ])];
                    post(ctx, channel_id, format_message(WATCH_NEW_VERSION, &lang, &[title, episode.to_string()]), rows).await;
                }
                config.mark_seen(&item.key);
            }
            Ok(false) => {
                if let Err(e) = repo.write_source(server_id, channel_id, episode, &item.link).await {
                    error = Some(e);
                    break;
                }
                // The release number is only worth repeating when it differs from the episode.
                let text = if release.season.is_some() || release.number == episode {
                    format_message(WATCH_SOURCE_SET_SAME, &lang, &[episode.to_string(), title])
                } else {
                    format_message(WATCH_SOURCE_SET, &lang, &[episode.to_string(), release.number.to_string(), title])
                };
                post(ctx, channel_id, text, vec![]).await;
                config.last_release = Some(item.title.clone());
                config.mark_seen(&item.key);
                written += 1;
            }
        }
    }
    config.last_error = error.clone();
    watch::save(server_id, channel_id, &config).await?;
    match error {
        Some(e) => Err(e),
        None => Ok(written),
    }
}

// Started once per process: `ready` fires again on every gateway reconnect.
pub fn start_watch_poller(ctx: Context) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        // Give the bot a minute to settle before the first round of requests.
        tokio::time::sleep(Duration::from_secs(60)).await;
        loop {
            for (server_id, channel_id) in watch::all() {
                match check_watch(&ctx, server_id, channel_id).await {
                    Ok(0) => {}
                    Ok(written) => println!("[watch] {} | {} new source(s)", channel_id, written),
                    Err(e) => println!("[watch] {} | {}", channel_id, e),
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}

// An `episode` typed as a release number, rewritten into the channel's own episode before the
// command sees it, so every command that takes an episode accepts both without each learning
// how. Only a number that cannot be an episode of this season is rewritten (see
// `watch::typed_episode`), and only in a channel whose watch has confirmed numbering. Returns
// `(typed, episode)` when it rewrote one, for `release_note`.
pub fn translate_release_episode(command: &mut serenity::all::CommandInteraction) -> Option<(u32, u32)> {
    if !RELEASE_EPISODE_COMMANDS.contains(&command.data.name.as_str()) {
        return None;
    }
    let server_id = command.guild_id?.get();
    let channel_id = command.channel_id.get();
    let meta = read_channel_meta(server_id, channel_id);
    let config = watch::load(server_id, channel_id)
        .filter(|config| config.confirmed && config.applies_to(meta.mal_id))?;
    let episode_count = meta.episode_count?;
    let option = episode_option_mut(&mut command.data.options)?;
    let typed = match option.value {
        CommandDataOptionValue::Integer(typed) => u32::try_from(typed).ok()?,
        _ => return None,
    };
    let episode = watch::typed_episode(typed, episode_count, config.offset)?;
    option.value = CommandDataOptionValue::Integer(episode as i64);
    Some((typed, episode))
}

fn episode_option_mut(options: &mut [CommandDataOption]) -> Option<&mut CommandDataOption> {
    let position = options.iter().position(|option| option.name == "episode");
    if let Some(position) = position {
        return options.get_mut(position);
    }
    options.iter_mut().find_map(|option| match &mut option.value {
        CommandDataOptionValue::SubCommand(inner) => episode_option_mut(inner),
        _ => None,
    })
}

// Says, to the person who typed it, which episode their release number was read as. Sent after
// the command has answered, since a follow-up needs an answer to follow.
pub async fn release_note(ctx: &Context, command: &serenity::all::CommandInteraction, typed: u32, episode: u32) {
    let text = command_format(command, WATCH_TYPED_RELEASE, &[typed.to_string(), episode.to_string()]);
    command
        .create_followup(ctx, CreateInteractionResponseFollowup::new().content(text).ephemeral(true))
        .await
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_ids_round_trip() {
        let config = WatchConfig { setup_message: Some(42), ..Default::default() };
        for (action, expected) in [
            ("ok", WatchAction::Confirm),
            ("cancel", WatchAction::Cancel),
            ("check", WatchAction::Check),
            ("stop", WatchAction::Stop),
            ("first", WatchAction::First),
            ("offset:63", WatchAction::Offset(63)),
            ("assign:abc123", WatchAction::Assign("abc123".to_string())),
            ("use:abc123:4", WatchAction::Use("abc123".to_string(), 4)),
            ("ignore:abc123", WatchAction::Ignore("abc123".to_string())),
        ] {
            let id = component_id(7, &config, action);
            assert!(id.len() <= 100, "{}", id);
            assert_eq!(parse_component_id(&id), Some((7, 42, expected)), "{}", id);
        }
        assert_eq!(parse_component_id("pnwatch:7:42:ok:extra"), None);
        assert_eq!(parse_component_id("pnbatch:7:42:ok"), None);
        assert_eq!(parse_component_id("pnwatch:7:42:offset:x"), None);
    }

    #[test]
    fn a_long_season_offers_the_window_around_the_release() {
        let values = |options: Vec<CreateSelectMenuOption>| {
            options
                .into_iter()
                .map(|option| serde_json::to_value(option).unwrap()["value"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        let short = values(episode_choices(80, 63, 12, "en"));
        assert_eq!(short.len(), 12);
        assert_eq!(short.first().map(String::as_str), Some("1"));

        let long = values(episode_choices(100, 0, 50, "en"));
        assert_eq!(long.len(), SELECT_LIMIT);
        assert_eq!(long.last().map(String::as_str), Some("50"));

        let middle = values(episode_choices(30, 0, 50, "en"));
        assert!(middle.contains(&"30".to_string()));
    }

    #[test]
    fn titles_are_clipped_without_backticks() {
        assert_eq!(clip("a`b", 10), "a'b");
        assert_eq!(clip("abcdef", 4), "abc…");
    }
}
