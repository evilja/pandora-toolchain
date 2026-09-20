use super::*;

use pandora_toolchain::lib::subs::ensure_ass_bytes;
use pandora_toolchain::pnworker::batch::{BatchEntry, BatchRequest};
use super::listing::SourceListing;
use pandora_toolchain::pnworker::messages::{
    BATCH_CANCELLED, BATCH_CONFIRM, BATCH_CONFIRM_BODY, BATCH_CONFIRM_EXPIRED, BATCH_MISMATCH,
    BATCH_PICK_PROMPT, FIELD_PROGRESS, PICK_TIMEOUT,
};
use pandora_toolchain::pnworker::probe_pages::{
    probe_page_body, probe_page_components, probe_page_count,
};
use serde::{Deserialize, Serialize};
use serenity::all::{ButtonStyle, Colour, ComponentInteraction, CreateActionRow, CreateButton};
use std::path::PathBuf;
use tokio::sync::mpsc::Sender;

const BATCH_COMPONENT_PREFIX: &str = "pnbatch";
// How long the file list waits for its index selection, the window every other pick gets.
const BATCH_PICK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

// The pairing survives the gap between the command and the confirmation click — including a pndc
// restart — by living on disk rather than in a pending-interaction map.
#[derive(Serialize, Deserialize)]
struct BatchPending {
    author: u64,
    channel_id: u64,
    server_id: Option<u64>,
    lang: String,
    probe_job_id: u64,
    source: String,
    entries: Vec<BatchPendingEntry>,
}

#[derive(Serialize, Deserialize)]
struct BatchPendingEntry {
    file_index: u64,
    file_label: String,
    subtitle_name: String,
}

fn pending_dir(message_id: u64) -> PathBuf {
    PathBuf::from("DB")
        .join("work")
        .join("batch-pending")
        .join(message_id.to_string())
}

// A subtitle archive handed to `/encode do`. The archive is the whole of what makes this a batch:
// several subtitles can only mean several episodes, so the source is listed here and paired
// against them, and nothing is queued until the pairing shown has been confirmed.
// An archive holding a single subtitle is just a subtitle that arrived zipped, and is encoded as
// one.
pub async fn handle_batch_link(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    tx: &Sender<JobClass>,
    torrent_url: String,
) {
    let Some(attachment) = option_attachment(command, "subtitle") else {
        command_error(ctx, command, "Error: Subtitle file is required").await;
        return;
    };
    let archive = match attachment.download().await {
        Ok(bytes) => bytes,
        Err(e) => {
            command_error(ctx, command, format!("Failed to download subtitles: {}", e)).await;
            return;
        }
    };
    // Answered before the archive is unpacked: converting a zip of SRTs runs ffmpeg once per entry,
    // and an interaction that has not been acknowledged within three seconds is gone.
    let Some(mut response) = working_response(ctx, command, "...").await else {
        return;
    };
    let mut subtitles = match read_subtitle_archive(&archive).await {
        Ok(subtitles) if !subtitles.is_empty() => subtitles,
        Ok(_) => {
            response_error(ctx, &mut response, "Error: the archive holds no subtitle files.").await;
            return;
        }
        Err(reason) => {
            response_error(ctx, &mut response, &format!("Error: {}", reason)).await;
            return;
        }
    };

    if subtitles.len() == 1 {
        if let Err(error) = response.react(ctx, '❌').await {
            report_send_failure("cancel reaction", response.channel_id.get(), &error);
        }
        let (_, subtitle) = subtitles.remove(0);
        let mut job = Job::new(
            command.user.id.get(),
            command.channel_id.get(),
            response.id.get(),
            JobType::Encode,
            response.id.get(),
            nyaaise(&torrent_url),
            subtitle,
            ctx.clone(),
            response,
            read_lang(command.guild_id),
            command.guild_id.map(|guild| guild.get()),
        );
        job.pick_file_first();
        tx.send(JobClass::Job(job)).await.unwrap();
        return;
    }

    if !is_listable_source(&torrent_url) {
        response_error(
            ctx,
            &mut response,
            "Error: a subtitle archive encodes several episodes, which needs a torrent or magnet holding them. A Google Drive or direct link is a single video — attach its one subtitle instead.",
        )
        .await;
        return;
    }
    let listing = match list_source(tx, command, &torrent_url).await {
        Ok(listing) => listing,
        Err(reason) => {
            response_error(ctx, &mut response, &format!("Error: {}", reason)).await;
            return;
        }
    };
    let Some(selection) =
        ask_which_files(ctx, command, &mut response, &listing, subtitles.len()).await
    else {
        return;
    };
    stage_batch(
        ctx,
        command,
        response,
        listing.probe_job_id,
        torrent_url,
        selection,
        subtitles,
    )
    .await;
}

// Which of the pack's files the archive is for. There is only a question when the pack holds more
// videos than the archive holds subtitles: with as many or fewer, every file is wanted and the
// confirmation that follows already reports any surplus. Otherwise the file list goes up with its
// page buttons and the requester answers in chat with an index selection — `1,3,5-9` — the same
// way a single file is picked. `None` means nobody answered, and the message already says so.
async fn ask_which_files(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    response: &mut Message,
    listing: &SourceListing,
    subtitle_count: usize,
) -> Option<Vec<(u64, String)>> {
    if listing.rows.len() <= subtitle_count {
        return Some(listing.rows.clone());
    }
    let lang = read_lang(command.guild_id);
    let embed = info_embed(command, BATCH_CONFIRM)
        .description(command_format(
            command,
            BATCH_PICK_PROMPT,
            &[listing.rows.len().to_string(), subtitle_count.to_string()],
        ))
        .field(
            get_message(FIELD_PROGRESS, &lang),
            probe_page_body(&listing.text, 1, &lang),
            false,
        );
    let _ = response
        .edit(
            ctx,
            EditMessage::new().content("").embed(embed).components(probe_page_components(
                listing.probe_job_id,
                1,
                probe_page_count(&listing.text),
            )),
        )
        .await;

    let rows = listing.rows.clone();
    let answer = await_pending_answer(
        command.user.id.get(),
        command.channel_id.get(),
        move |text| is_index_selection(text) && select_files(&rows, Some(text)).is_ok_and(|files| !files.is_empty()),
        BATCH_PICK_TIMEOUT,
    )
    .await;
    match answer {
        Some(text) => select_files(&listing.rows, Some(&text)).ok(),
        None => {
            let _ = response
                .edit(
                    ctx,
                    EditMessage::new()
                        .content(get_message(PICK_TIMEOUT, &lang))
                        .embeds(vec![])
                        .components(vec![]),
                )
                .await;
            None
        }
    }
}

// Whether a chat message is an index selection and nothing else. `select_files` skips empty parts,
// so without this a message of commas — or an ordinary sentence that happened to parse — could be
// taken for an answer.
fn is_index_selection(text: &str) -> bool {
    let text = text.trim();
    text.chars().any(|character| character.is_ascii_digit())
        && text
            .chars()
            .all(|character| character.is_ascii_digit() || matches!(character, ',' | '-' | ' '))
}

// Whether a `/encode do` subtitle attachment is an archive rather than a subtitle. Decided by name,
// like every other subtitle format is: the bytes have not been downloaded yet when this is asked.
pub fn is_subtitle_archive(command: &serenity::all::CommandInteraction) -> bool {
    option_attachment(command, "subtitle")
        .map(|attachment| attachment.filename.to_ascii_lowercase().ends_with(".zip"))
        .unwrap_or(false)
}

async fn response_error(ctx: &Context, response: &mut Message, text: &str) {
    let _ = response.edit(ctx, EditMessage::new().content(text)).await;
}

// Stages a pairing on disk and turns the response into its confirmation.
async fn stage_batch(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    response: Message,
    probe_job_id: u64,
    source: String,
    selection: Vec<(u64, String)>,
    subtitles: Vec<(String, Vec<u8>)>,
) {
    let lang = read_lang(command.guild_id);
    let pairs = selection.len().min(subtitles.len());
    let mut notice = String::new();
    if selection.len() != subtitles.len() {
        notice = format!(
            "\n{}",
            command_format(
                command,
                BATCH_MISMATCH,
                &[
                    selection.len().to_string(),
                    subtitles.len().to_string(),
                    pairs.to_string(),
                ],
            )
        );
    }

    let listing = pairing_lines(&selection[..pairs], &subtitles[..pairs]).join("\n");

    let pending = BatchPending {
        author: command.user.id.get(),
        channel_id: command.channel_id.get(),
        server_id: command.guild_id.map(|guild| guild.get()),
        lang: lang.clone(),
        probe_job_id,
        source: source.clone(),
        entries: selection[..pairs]
            .iter()
            .zip(subtitles[..pairs].iter())
            .map(|((index, label), (name, _))| BatchPendingEntry {
                file_index: *index,
                file_label: label.clone(),
                subtitle_name: name.clone(),
            })
            .collect(),
    };
    let mut response = response;
    if let Err(e) = write_pending(response.id.get(), &pending, &subtitles[..pairs]).await {
        response_error(ctx, &mut response, &format!("Error: {}", e)).await;
        return;
    }

    let total_pages = probe_page_count(&listing);
    let embed = info_embed(command, BATCH_CONFIRM)
        .description(format!(
            "{}{}\n\n{}",
            command_format(command, BATCH_CONFIRM_BODY, &[pairs.to_string()]),
            notice,
            probe_page_body(&listing, 1, &lang),
        ));
    let _ = response
        .edit(
            ctx,
            EditMessage::new()
                .content("")
                .embed(embed)
                .components(batch_components(response.id.get(), 1, total_pages)),
        )
        .await;
}

// Paging, confirming, and cancelling all rewrite the same message; nothing is kept in memory, so a
// restart between the command and the click only costs the click.
pub async fn handle_batch_component(
    ctx: &Context,
    component: &ComponentInteraction,
    tx: &Sender<JobClass>,
) {
    let Some((message_id, action)) = parse_batch_component_id(&component.data.custom_id) else {
        let _ = component
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await;
        return;
    };
    let lang = read_lang(component.guild_id);
    let Some(pending) = read_pending(message_id).await else {
        component
            .create_response(
                ctx,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(get_message(BATCH_CONFIRM_EXPIRED, &lang))
                        .ephemeral(true),
                ),
            )
            .await
            .ok();
        return;
    };
    if component.user.id.get() != pending.author {
        component
            .create_response(
                ctx,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(get_message(BATCH_CONFIRM_EXPIRED, &lang))
                        .ephemeral(true),
                ),
            )
            .await
            .ok();
        return;
    }

    match action {
        BatchAction::Page(page) => {
            let listing = pending
                .entries
                .iter()
                .map(|entry| format!("`{}` {} → {}", entry.file_index, entry.file_label, entry.subtitle_name))
                .collect::<Vec<_>>()
                .join("\n");
            let total_pages = probe_page_count(&listing);
            let page = page.clamp(1, total_pages.max(1));
            let Some(embed) = component.message.embeds.first() else {
                let _ = component
                    .create_response(ctx, CreateInteractionResponse::Acknowledge)
                    .await;
                return;
            };
            let head = embed
                .description
                .clone()
                .unwrap_or_default()
                .split("\n\n")
                .next()
                .unwrap_or_default()
                .to_string();
            let rebuilt = CreateEmbed::new()
                .title(embed.title.clone().unwrap_or_default())
                .colour(embed.colour.unwrap_or(Colour::BLUE))
                .description(format!(
                    "{}\n\n{}",
                    head,
                    probe_page_body(&listing, page, &pending.lang)
                ))
                .timestamp(serenity::model::Timestamp::now());
            component
                .create_response(
                    ctx,
                    CreateInteractionResponse::UpdateMessage(
                        CreateInteractionResponseMessage::new()
                            .embed(rebuilt)
                            .components(batch_components(message_id, page, total_pages)),
                    ),
                )
                .await
                .ok();
        }
        BatchAction::Cancel => {
            remove_pending(message_id).await;
            component
                .create_response(
                    ctx,
                    CreateInteractionResponse::UpdateMessage(
                        CreateInteractionResponseMessage::new()
                            .content(get_message(BATCH_CANCELLED, &pending.lang))
                            .embeds(vec![])
                            .components(vec![]),
                    ),
                )
                .await
                .ok();
        }
        BatchAction::Confirm => {
            let mut entries = Vec::new();
            for (position, entry) in pending.entries.iter().enumerate() {
                let Ok(subtitle) = tokio::fs::read(subtitle_path(message_id, position)).await else {
                    continue;
                };
                entries.push(BatchEntry {
                    file_index: entry.file_index,
                    file_label: entry.file_label.clone(),
                    subtitle_name: entry.subtitle_name.clone(),
                    subtitle,
                    job_id: None,
                });
            }
            if entries.is_empty() {
                component
                    .create_response(
                        ctx,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content(get_message(BATCH_CONFIRM_EXPIRED, &pending.lang))
                                .ephemeral(true),
                        ),
                    )
                    .await
                    .ok();
                return;
            }
            component
                .create_response(
                    ctx,
                    CreateInteractionResponse::UpdateMessage(
                        CreateInteractionResponseMessage::new()
                            .content("")
                            .embeds(vec![])
                            .components(vec![]),
                    ),
                )
                .await
                .ok();
            let mut job = Job::new(
                pending.author,
                pending.channel_id,
                message_id,
                JobType::Batch,
                message_id,
                nyaaise(&pending.source),
                Vec::new(),
                ctx.clone(),
                (*component.message).clone(),
                pending.lang.clone(),
                pending.server_id,
            );
            job.display_link = Some(display_source_link(&pending.source));
            job.probe_job_id = Some(pending.probe_job_id);
            job.batch = Some(BatchRequest::new(entries, pending.probe_job_id));
            remove_pending(message_id).await;
            tx.send(JobClass::Job(job)).await.unwrap();
        }
    }
}

enum BatchAction {
    Page(usize),
    Confirm,
    Cancel,
}

fn batch_components(message_id: u64, page: usize, total_pages: usize) -> Vec<CreateActionRow> {
    let mut rows = Vec::new();
    if total_pages > 1 {
        let page = page.clamp(1, total_pages);
        rows.push(CreateActionRow::Buttons(vec![
            CreateButton::new(format!(
                "{}:{}:page:{}",
                BATCH_COMPONENT_PREFIX,
                message_id,
                page.saturating_sub(1).max(1)
            ))
            .label("◀")
            .style(ButtonStyle::Secondary)
            .disabled(page == 1),
            CreateButton::new(format!(
                "{}:{}:page:{}",
                BATCH_COMPONENT_PREFIX,
                message_id,
                (page + 1).min(total_pages)
            ))
            .label("▶")
            .style(ButtonStyle::Secondary)
            .disabled(page == total_pages),
        ]));
    }
    rows.push(CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{}:{}:confirm", BATCH_COMPONENT_PREFIX, message_id))
            .label("✅")
            .style(ButtonStyle::Success),
        CreateButton::new(format!("{}:{}:cancel", BATCH_COMPONENT_PREFIX, message_id))
            .label("✖")
            .style(ButtonStyle::Danger),
    ]));
    rows
}

fn parse_batch_component_id(id: &str) -> Option<(u64, BatchAction)> {
    let mut parts = id.split(':');
    if parts.next()? != BATCH_COMPONENT_PREFIX {
        return None;
    }
    let message_id = parts.next()?.parse().ok()?;
    let action = match parts.next()? {
        "confirm" => BatchAction::Confirm,
        "cancel" => BatchAction::Cancel,
        "page" => BatchAction::Page(parts.next()?.parse().ok()?),
        _ => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((message_id, action))
}

// The probe worker already renders its rows episode-sorted, so reading that stored list back keeps the
// pairing in the order the user was shown rather than the order the torrent packed its files.
pub(super) fn probe_rows(progress: Option<&str>) -> Vec<(u64, String)> {
    let Some(progress) = progress else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(progress) else {
        return Vec::new();
    };
    if value.get("type").and_then(|value| value.as_str()) != Some("probe") {
        return Vec::new();
    }
    // The rendered list, under the key it is written to today and the one older rows used. `files`
    // has since become the structured array beside it, which is why asking only for that as a
    // string found nothing and reported every probe as having produced no files.
    let text = value
        .get("file_text")
        .and_then(|value| value.as_str())
        .or_else(|| value.get("files").and_then(|value| value.as_str()));
    if let Some(text) = text {
        let rows = probe_rows_from_text(text);
        if !rows.is_empty() {
            return rows;
        }
    }
    // Nothing rendered to read: fall back to the structured list. Its `name` is the file's real
    // name rather than the episode label the rows were rendered with, so this is the second choice
    // and not the first — a batch names its children after what the user was shown.
    value
        .get("files")
        .or_else(|| value.get("file_options"))
        .and_then(|value| value.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let index = entry.get("index").and_then(|value| value.as_u64())?;
                    let name = entry.get("name").and_then(|value| value.as_str())?;
                    Some((index, name.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn probe_rows_from_text(text: &str) -> Vec<(u64, String)> {
    text.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix('`')?;
            let end = rest.find('`')?;
            let index = rest[..end].trim().parse::<u64>().ok()?;
            let label = rest[end + 1..]
                .trim_start()
                .trim_start_matches('—')
                .trim()
                .replace('`', "");
            Some((index, label))
        })
        .collect()
}

// `1,3,5-9` in probe-index terms. The result keeps the probe's episode order, not the order the
// user typed, so the pairing shown is the pairing that runs.
fn select_files(
    files: &[(u64, String)],
    selection: Option<&str>,
) -> Result<Vec<(u64, String)>, String> {
    let Some(selection) = selection else {
        return Ok(files.to_vec());
    };
    let mut wanted: Vec<u64> = Vec::new();
    for part in selection.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('-') {
            Some((start, end)) => {
                let start = start.trim().parse::<u64>().map_err(|_| {
                    format!("Error: `{}` is not a valid index range.", part)
                })?;
                let end = end
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| format!("Error: `{}` is not a valid index range.", part))?;
                if end < start || end.saturating_sub(start) > 512 {
                    return Err(format!("Error: `{}` is not a valid index range.", part));
                }
                wanted.extend(start..=end);
            }
            None => wanted.push(
                part.parse::<u64>()
                    .map_err(|_| format!("Error: `{}` is not a valid index.", part))?,
            ),
        }
    }
    Ok(files
        .iter()
        .filter(|(index, _)| wanted.contains(index))
        .cloned()
        .collect())
}

// Subtitles are normalised to ASS here rather than at queue time: a batch child is handed a ready
// work directory, so a bad entry has to be caught while the user is still looking at a prompt.
async fn read_subtitle_archive(archive: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    use futures_lite::AsyncReadExt;

    let reader = async_zip::base::read::mem::ZipFileReader::new(archive.to_vec())
        .await
        .map_err(|e| format!("the archive could not be read: {}", e))?;
    let mut named: Vec<(String, usize)> = Vec::new();
    for (position, entry) in reader.file().entries().iter().enumerate() {
        let Ok(name) = entry.filename().as_str() else {
            continue;
        };
        if name.ends_with('/') {
            continue;
        }
        let base = name.rsplit('/').next().unwrap_or(name).to_string();
        if base.starts_with('.') || !pandora_toolchain::lib::subs::is_subtitle_name(&base) {
            continue;
        }
        named.push((base, position));
    }
    named.sort_by(|(left, _), (right, _)| natural_key(left).cmp(&natural_key(right)));

    let mut subtitles = Vec::new();
    for (name, position) in named {
        let mut bytes = Vec::new();
        reader
            .reader_with_entry(position)
            .await
            .map_err(|e| format!("`{}` could not be read: {}", name, e))?
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| format!("`{}` could not be read: {}", name, e))?;
        let converted = ensure_ass_bytes(&bytes)
            .await
            .map_err(|e| format!("`{}`: {}", name, e))?;
        subtitles.push((name, converted.bytes));
    }
    Ok(subtitles)
}

// `10.ass` must sort after `2.ass`, so digit runs compare as numbers and everything else compares
// case-insensitively as text.
fn natural_key(name: &str) -> Vec<(u64, String)> {
    let mut key = Vec::new();
    let mut characters = name.chars().peekable();
    while let Some(character) = characters.next() {
        if character.is_ascii_digit() {
            let mut digits = character.to_string();
            while characters.peek().is_some_and(|next| next.is_ascii_digit()) {
                digits.push(characters.next().unwrap());
            }
            key.push((digits.parse::<u64>().unwrap_or(u64::MAX), String::new()));
        } else {
            let mut text = character.to_lowercase().to_string();
            while characters
                .peek()
                .is_some_and(|next| !next.is_ascii_digit())
            {
                text.push_str(&characters.next().unwrap().to_lowercase().to_string());
            }
            key.push((u64::MAX, text));
        }
    }
    key
}

fn pairing_lines(files: &[(u64, String)], subtitles: &[(String, Vec<u8>)]) -> Vec<String> {
    files
        .iter()
        .zip(subtitles.iter())
        .map(|((index, label), (name, _))| format!("`{}` {} → {}", index, label, name))
        .collect()
}

fn subtitle_path(message_id: u64, position: usize) -> PathBuf {
    pending_dir(message_id)
        .join("subs")
        .join(format!("{}.ass", position))
}

async fn write_pending(
    message_id: u64,
    pending: &BatchPending,
    subtitles: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let directory = pending_dir(message_id);
    tokio::fs::create_dir_all(directory.join("subs"))
        .await
        .map_err(|e| format!("the batch could not be staged: {}", e))?;
    for (position, (_, bytes)) in subtitles.iter().enumerate() {
        tokio::fs::write(subtitle_path(message_id, position), bytes)
            .await
            .map_err(|e| format!("the batch could not be staged: {}", e))?;
    }
    let manifest = serde_json::to_string(pending)
        .map_err(|e| format!("the batch could not be staged: {}", e))?;
    tokio::fs::write(directory.join("manifest.json"), manifest)
        .await
        .map_err(|e| format!("the batch could not be staged: {}", e))
}

async fn read_pending(message_id: u64) -> Option<BatchPending> {
    let manifest = tokio::fs::read_to_string(pending_dir(message_id).join("manifest.json"))
        .await
        .ok()?;
    serde_json::from_str(&manifest).ok()
}

async fn remove_pending(message_id: u64) {
    tokio::fs::remove_dir_all(pending_dir(message_id)).await.ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_index_selection_answers_the_which_files_question() {
        assert!(is_index_selection("1,3,5-9"));
        assert!(is_index_selection(" 4 "));
        assert!(is_index_selection("1, 2 - 4"));
        assert!(!is_index_selection(", , -"));
        assert!(!is_index_selection("episodes 1-3 please"));
        assert!(!is_index_selection(""));
    }

    #[test]
    fn probe_rows_keep_the_episode_sorted_order_and_index() {
        let progress = serde_json::json!({
            "type": "probe",
            "files": "`3` — E01\n`1` — E02\n`7` — Specials (420MB)",
        })
        .to_string();
        assert_eq!(
            probe_rows(Some(&progress)),
            vec![
                (3, "E01".to_string()),
                (1, "E02".to_string()),
                (7, "Specials (420MB)".to_string()),
            ]
        );
    }

    // The shape a probe actually writes: the rendered lines under `file_text`, with `files` beside
    // them as the structured array. Reading `files` as a string here is what made a batch
    // answer "that job has no probed file list" after every probe.
    #[test]
    fn probe_rows_read_the_list_a_probe_writes_today() {
        let progress = serde_json::json!({
            "type": "probe",
            "file_text": "`3` — E01\n`1` — E02",
            "files": [
                { "index": 3, "name": "[Group] Show - 01.mkv", "bytes": 1 },
                { "index": 1, "name": "[Group] Show - 02.mkv", "bytes": 2 },
            ],
            "file_options": [],
        })
        .to_string();
        // The episode labels the user was shown, not the file names beside them.
        assert_eq!(
            probe_rows(Some(&progress)),
            vec![(3, "E01".to_string()), (1, "E02".to_string())]
        );
    }

    // A row with no rendered text at all still names its episodes, from the structured list.
    #[test]
    fn probe_rows_fall_back_to_the_structured_list() {
        let progress = serde_json::json!({
            "type": "probe",
            "files": [{ "index": 7, "name": "Specials.mkv", "bytes": 3 }],
        })
        .to_string();
        assert_eq!(
            probe_rows(Some(&progress)),
            vec![(7, "Specials.mkv".to_string())]
        );
    }

    #[test]
    fn selection_accepts_lists_and_ranges_in_probe_order() {
        let files = vec![
            (3, "E01".to_string()),
            (1, "E02".to_string()),
            (7, "E03".to_string()),
        ];
        assert_eq!(select_files(&files, None).unwrap().len(), 3);
        assert_eq!(
            select_files(&files, Some("7,1")).unwrap(),
            vec![(1, "E02".to_string()), (7, "E03".to_string())]
        );
        assert_eq!(select_files(&files, Some("1-3")).unwrap().len(), 2);
        assert!(select_files(&files, Some("nope")).is_err());
    }

    #[test]
    fn subtitles_sort_numerically_not_lexicographically() {
        let mut names = vec!["10.ass", "2.ass", "1.ass"];
        names.sort_by(|left, right| natural_key(left).cmp(&natural_key(right)));
        assert_eq!(names, vec!["1.ass", "2.ass", "10.ass"]);
    }

    #[test]
    fn component_ids_round_trip() {
        assert!(matches!(
            parse_batch_component_id("pnbatch:42:page:3"),
            Some((42, BatchAction::Page(3)))
        ));
        assert!(matches!(
            parse_batch_component_id("pnbatch:42:confirm"),
            Some((42, BatchAction::Confirm))
        ));
        assert!(parse_batch_component_id("pnprobe:42:1").is_none());
        assert!(parse_batch_component_id("pnbatch:42:confirm:extra").is_none());
    }

    #[test]
    fn pairing_is_positional_over_the_shown_order() {
        let files = vec![(3, "E01".to_string()), (1, "E02".to_string())];
        let subtitles = vec![
            ("01.ass".to_string(), Vec::new()),
            ("02.ass".to_string(), Vec::new()),
        ];
        assert_eq!(
            pairing_lines(&files, &subtitles),
            vec!["`3` E01 → 01.ass".to_string(), "`1` E02 → 02.ass".to_string()]
        );
    }
}
