use super::*;

use pandora_toolchain::lib::http::mal::prequel_episode_total;
use pandora_toolchain::lib::source_doc::{compose as compose_source, ProbeRef};
use pandora_toolchain::lib::watch::{self, PackFile, PackPlan};
use serenity::all::{ButtonStyle, Colour, ComponentInteraction, CreateActionRow, CreateButton};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, Sender, UnboundedSender};

const SOURCE_COMPONENT_PREFIX: &str = "pnsource";
// A preview is read row by row against the pack, so it waits longer than a typed index does.
const PREVIEW_TIMEOUT: Duration = Duration::from_secs(600);
// Rows drawn in the preview. A season that runs longer still has every episode written; the rest
// are counted rather than listed, so the embed stays inside Discord's limit.
const PREVIEW_ROWS: usize = 40;
// Discord's own limit on the options of one select menu.
const SELECT_LIMIT: usize = 25;

enum Choice {
    All,
    Missing,
    Cancel,
    First(u32),
}

// The preview a command is waiting on, keyed by the message it is drawn in. Its buttons are only
// the author's to press, like a typed pick is only the author's to answer.
struct Waiting {
    author: u64,
    choices: UnboundedSender<Choice>,
}

fn waiting() -> &'static Mutex<HashMap<u64, Waiting>> {
    static WAITING: OnceLock<Mutex<HashMap<u64, Waiting>>> = OnceLock::new();
    WAITING.get_or_init(|| Mutex::new(HashMap::new()))
}

enum Outcome {
    Write(PackPlan, bool),
    Cancelled,
    TimedOut,
    Failed(String),
}

// `/source link:<pack>` with no episode: every episode the pack holds, each written with the file
// it is. The probe already numbers a pack's files by the number that counts up across them, so
// this is `/source` answering its own pick once per episode, with the numbering shown first for a
// person to confirm — a wrong offset here would point a whole season at the wrong files.
pub async fn handle_source_batch(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    tx: &Sender<JobClass>,
) {
    let Some(link) = required_trimmed_option(ctx, command, "link", "Source link").await else {
        return;
    };
    let Some(server_id) = command_server_id(ctx, command, "/source").await else {
        return;
    };
    if !is_listable_source(&link) {
        command_error(ctx, command, command_message(command, SOURCE_BATCH_NEEDS_PACK)).await;
        return;
    }
    let Some((meta, owner_repo, repo_url)) = attached_repo(ctx, command, server_id, None).await else {
        return;
    };
    let Some((forgejo_base, api_key)) = forgejo_config(ctx, command, server_id).await else {
        return;
    };
    let Some(mut response_msg) = working_response(ctx, command, "Working…").await else {
        return;
    };
    let fg = match Forgejo::new(forgejo_base, api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Forgejo init failed: {}", e))).await;
            return;
        }
    };
    let lang = command_language(command);
    let episode_count = meta.episode_count.unwrap_or(0);

    let listing = match list_source(tx, command, &link).await {
        Ok(listing) => listing,
        Err(reason) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Error: {}", reason))).await;
            return;
        }
    };
    if listing.rows.len() < 2 {
        let _ = response_msg
            .edit(ctx, EditMessage::new().content(get_message(SOURCE_BATCH_ONE_FILE, &lang)))
            .await;
        return;
    }
    let files: Vec<PackFile> = listing
        .rows
        .iter()
        .filter_map(|(index, label)| watch::pack_file(*index, label))
        .collect();
    if files.is_empty() {
        let _ = response_msg
            .edit(ctx, EditMessage::new().content(get_message(SOURCE_BATCH_NONE_NUMBERED, &lang)))
            .await;
        return;
    }
    let unnumbered = listing.rows.len() - files.len();

    // The same guess `/watch` makes of a feed: numbers that already fit are episodes, and numbers
    // past the season are counted on from the earlier seasons when MyAnimeList says how many.
    let numbers: Vec<u32> = files.iter().map(|file| file.number).collect();
    let past_the_season = numbers.iter().any(|number| *number > episode_count);
    let prequel_total = match (past_the_season, meta.mal_id) {
        (true, Some(mal_id)) => prequel_episode_total(mal_id).await,
        _ => None,
    };
    let mut offset = watch::guess_offset(&numbers, episode_count, prequel_total);

    let link_display = if link.starts_with("magnet:") {
        get_message(VALUE_MAGNET_HIDDEN, &lang)
    } else {
        source_link(&link)
    };
    let message_id = response_msg.id.get();
    let (sender, mut choices) = unbounded_channel();
    waiting()
        .lock()
        .unwrap()
        .insert(message_id, Waiting { author: command.user.id.get(), choices: sender });

    // Which episodes already have a source, asked once per episode however often the offset moves.
    let mut existing: HashMap<u32, bool> = HashMap::new();
    let outcome = loop {
        let plan = watch::plan_pack(&files, episode_count, offset);
        if let Err(e) = fill_existing(&fg, &owner_repo, &plan, &mut existing).await {
            break Outcome::Failed(e);
        }
        let (embed, rows) = preview(&lang, &link_display, &files, &plan, unnumbered, offset, &existing, episode_count);
        let _ = response_msg
            .edit(ctx, EditMessage::new().content("").embed(embed).components(rows))
            .await;
        match tokio::time::timeout(PREVIEW_TIMEOUT, choices.recv()).await {
            Ok(Some(Choice::First(first))) => offset = first.saturating_sub(1),
            Ok(Some(Choice::All)) => break Outcome::Write(plan, false),
            Ok(Some(Choice::Missing)) => break Outcome::Write(plan, true),
            Ok(Some(Choice::Cancel)) | Ok(None) => break Outcome::Cancelled,
            Err(_) => break Outcome::TimedOut,
        }
    };
    waiting().lock().unwrap().remove(&message_id);

    let (plan, only_missing) = match outcome {
        Outcome::Write(plan, only_missing) => (plan, only_missing),
        Outcome::Cancelled => return close_preview(ctx, &mut response_msg, get_message(SOURCE_BATCH_CANCELLED, &lang)).await,
        Outcome::TimedOut => return close_preview(ctx, &mut response_msg, get_message(SOURCE_BATCH_TIMEOUT, &lang)).await,
        Outcome::Failed(e) => return close_preview(ctx, &mut response_msg, format!("Error: {}", e)).await,
    };

    let targets: Vec<(u32, PackFile)> = plan
        .episodes
        .into_iter()
        .filter(|(episode, _)| !only_missing || !existing.get(episode).copied().unwrap_or(false))
        .collect();
    let _ = response_msg
        .edit(
            ctx,
            EditMessage::new()
                .content(format_message(SOURCE_BATCH_WRITING, &lang, &[targets.len().to_string()]))
                .embeds(vec![])
                .components(vec![]),
        )
        .await;

    let source = source_link(&link);
    let mut written = Vec::new();
    let mut failure = None;
    for (episode, file) in &targets {
        let path = format!("{}/SOURCE.md", pad2(*episode));
        let probe = ProbeRef { job_id: listing.probe_job_id, file_index: file.index };
        let content = compose_source(&source, Some(probe));
        match fg.upsert_file(&owner_repo, &path, &base64_encode(&content), "Set source link (pack)").await {
            Ok(()) => {
                remove_gitkeep_for_path(&fg, &owner_repo, &path).await;
                written.push(*episode);
            }
            Err(e) => {
                failure = Some(format!("`{}`: {}", path, e));
                break;
            }
        }
    }
    if !written.is_empty() {
        pandora_toolchain::lib::git::record_attachment_sync(server_id, command.channel_id.get()).await;
    }

    let mut description = format_message(
        SOURCE_BATCH_DONE,
        &lang,
        &[written.len().to_string(), episode_ranges(&written)],
    );
    if let Some(failure) = &failure {
        description.push_str("\n\n");
        description.push_str(&format_message(SOURCE_BATCH_FAILED, &lang, &[failure.clone()]));
    }
    let embed = match failure {
        Some(_) => info_embed(command, COMMAND_SOURCE_UPDATED),
        None => success_embed(command, COMMAND_SOURCE_UPDATED),
    };
    let embed = embed
        .description(description)
        .field(command_message(command, FIELD_REPO), format!("[{}]({})", owner_repo, repo_url), true)
        .field(command_message(command, FIELD_SOURCE), link_display, false);
    let _ = response_msg
        .edit(ctx, EditMessage::new().content("").embed(embed).components(vec![]))
        .await;
}

async fn close_preview(ctx: &Context, response_msg: &mut Message, text: String) {
    let _ = response_msg
        .edit(ctx, EditMessage::new().content(text).embeds(vec![]).components(vec![]))
        .await;
}

async fn fill_existing(
    fg: &Forgejo,
    owner_repo: &str,
    plan: &PackPlan,
    existing: &mut HashMap<u32, bool>,
) -> Result<(), String> {
    for (episode, _) in &plan.episodes {
        if existing.contains_key(episode) {
            continue;
        }
        let path = format!("{}/SOURCE.md", pad2(*episode));
        let has = fg.get_file_content(owner_repo, &path).await?.is_some();
        existing.insert(*episode, has);
    }
    Ok(())
}

fn file_label(file: &PackFile) -> String {
    if file.version > 1 {
        format!("E{}v{}", file.number, file.version)
    } else {
        format!("E{}", file.number)
    }
}

// `1–5, 7, 9–12`: a season's worth of episode numbers in a line that can still be read.
fn episode_ranges(episodes: &[u32]) -> String {
    let mut sorted = episodes.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        parts.push(if start == end { start.to_string() } else { format!("{}–{}", start, end) });
        i += 1;
    }
    parts.join(", ")
}

fn preview(
    lang: &str,
    link_display: &str,
    files: &[PackFile],
    plan: &PackPlan,
    unnumbered: usize,
    offset: u32,
    existing: &HashMap<u32, bool>,
    episode_count: u32,
) -> (CreateEmbed, Vec<CreateActionRow>) {
    let mut description = format_message(
        SOURCE_BATCH_BODY,
        lang,
        &[link_display.to_string(), files.len().to_string()],
    );
    description.push_str("\n\n");
    description.push_str(&if offset == 0 {
        get_message(SOURCE_BATCH_NUMBERING_SAME, lang)
    } else {
        format_message(SOURCE_BATCH_NUMBERING_OFFSET, lang, &[format!("E{}", offset + 1)])
    });
    description.push_str("\n\n");
    if plan.episodes.is_empty() {
        description.push_str(&get_message(SOURCE_BATCH_NO_EPISODES, lang));
    } else {
        let rows: Vec<String> = plan
            .episodes
            .iter()
            .take(PREVIEW_ROWS)
            .map(|(episode, file)| {
                let replaces = existing.get(episode).copied().unwrap_or(false);
                let id = if replaces { SOURCE_BATCH_ROW_REPLACES } else { SOURCE_BATCH_ROW };
                format_message(id, lang, &[episode.to_string(), file_label(file), file.index.to_string()])
            })
            .collect();
        description.push_str(&rows.join("\n"));
        if plan.episodes.len() > PREVIEW_ROWS {
            description.push('\n');
            description.push_str(&format_message(
                SOURCE_BATCH_MORE,
                lang,
                &[(plan.episodes.len() - PREVIEW_ROWS).to_string()],
            ));
        }
    }

    let mut notes = Vec::new();
    let absent: Vec<u32> = (1..=episode_count)
        .filter(|episode| !plan.episodes.iter().any(|(taken, _)| taken == episode))
        .collect();
    if !absent.is_empty() && !plan.episodes.is_empty() {
        notes.push(format_message(SOURCE_BATCH_NOT_IN_PACK, lang, &[episode_ranges(&absent)]));
    }
    if !plan.outside.is_empty() {
        let mut shown: Vec<String> = plan.outside.iter().take(12).map(|number| format!("E{}", number)).collect();
        if plan.outside.len() > 12 {
            shown.push("…".to_string());
        }
        notes.push(format_message(SOURCE_BATCH_OUTSIDE, lang, &[shown.join(", ")]));
    }
    if !plan.duplicates.is_empty() {
        notes.push(format_message(SOURCE_BATCH_DUPLICATES, lang, &[episode_ranges(&plan.duplicates)]));
    }
    if unnumbered > 0 {
        notes.push(format_message(SOURCE_BATCH_UNNUMBERED, lang, &[unnumbered.to_string()]));
    }
    if !notes.is_empty() {
        description.push_str("\n\n");
        description.push_str(&notes.join("\n"));
    }

    let embed = CreateEmbed::new()
        .title(get_message(SOURCE_BATCH_TITLE, lang))
        .colour(Colour::BLUE)
        .description(description);

    let id = |action: &str| format!("{}:{}", SOURCE_COMPONENT_PREFIX, action);
    let total = plan.episodes.len();
    let missing = plan
        .episodes
        .iter()
        .filter(|(episode, _)| !existing.get(episode).copied().unwrap_or(false))
        .count();
    let mut buttons = Vec::new();
    if total > 0 {
        buttons.push(
            CreateButton::new(id("all"))
                .label(format_message(SOURCE_BATCH_WRITE_ALL, lang, &[total.to_string()]))
                .style(ButtonStyle::Success),
        );
    }
    if missing > 0 && missing < total {
        buttons.push(
            CreateButton::new(id("missing"))
                .label(format_message(SOURCE_BATCH_WRITE_MISSING, lang, &[missing.to_string()]))
                .style(ButtonStyle::Primary),
        );
    }
    buttons.push(
        CreateButton::new(id("cancel"))
            .label(get_message(SOURCE_BATCH_CANCEL, lang))
            .style(ButtonStyle::Danger),
    );
    let mut rows = vec![CreateActionRow::Buttons(buttons)];

    // Picking the file that is episode 1 is the whole of changing the numbering, as in `/watch`.
    let mut numbers: Vec<u32> = files.iter().map(|file| file.number).collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers.truncate(SELECT_LIMIT);
    if numbers.len() > 1 || offset != 0 {
        let current = offset + 1;
        let options = numbers
            .into_iter()
            .map(|number| {
                CreateSelectMenuOption::new(
                    format_message(SOURCE_BATCH_FIRST_OPTION, lang, &[format!("E{}", number)]),
                    number.to_string(),
                )
                .default_selection(number == current)
            })
            .collect();
        rows.push(CreateActionRow::SelectMenu(
            CreateSelectMenu::new(id("first"), CreateSelectMenuKind::String { options })
                .placeholder(get_message(SOURCE_BATCH_PICK_FIRST, lang)),
        ));
    }
    (embed, rows)
}

pub async fn handle_source_component(ctx: &Context, component: &ComponentInteraction) {
    let lang = read_lang(component.guild_id);
    let action = component
        .data
        .custom_id
        .strip_prefix(SOURCE_COMPONENT_PREFIX)
        .and_then(|rest| rest.strip_prefix(':'))
        .unwrap_or("");
    let choice = match action {
        "all" => Some(Choice::All),
        "missing" => Some(Choice::Missing),
        "cancel" => Some(Choice::Cancel),
        "first" => match &component.data.kind {
            ComponentInteractionDataKind::StringSelect { values } => {
                values.first().and_then(|value| value.parse::<u32>().ok()).map(Choice::First)
            }
            _ => None,
        },
        _ => None,
    };
    let Some(choice) = choice else {
        let _ = component.create_response(ctx, CreateInteractionResponse::Acknowledge).await;
        return;
    };
    let refusal = {
        let waiting = waiting().lock().unwrap();
        match waiting.get(&component.message.id.get()) {
            None => Some(SOURCE_BATCH_EXPIRED),
            Some(preview) if preview.author != component.user.id.get() => Some(SOURCE_BATCH_NOT_YOURS),
            Some(preview) => preview.choices.send(choice).err().map(|_| SOURCE_BATCH_EXPIRED),
        }
    };
    let response = match refusal {
        // The waiting command redraws the message itself.
        None => CreateInteractionResponse::Acknowledge,
        Some(id) => CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new().content(get_message(id, &lang)).ephemeral(true),
        ),
    };
    let _ = component.create_response(ctx, response).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_ranges_collapse_runs() {
        assert_eq!(episode_ranges(&[3, 1, 2, 5, 7, 8, 9]), "1–3, 5, 7–9");
        assert_eq!(episode_ranges(&[4]), "4");
        assert_eq!(episode_ranges(&[]), "");
    }
}
