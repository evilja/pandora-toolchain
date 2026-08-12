use super::*;

use pandora_toolchain::lib::subs::ensure_ass_bytes;
use pandora_toolchain::pnworker::batch::{BatchEntry, BatchRequest};
use pandora_toolchain::pnworker::messages::{
    BATCH_CANCELLED, BATCH_CONFIRM, BATCH_CONFIRM_BODY, BATCH_CONFIRM_EXPIRED, BATCH_MISMATCH,
};
use pandora_toolchain::pnworker::probe_pages::{probe_page_body, probe_page_count};
use serde::{Deserialize, Serialize};
use serenity::all::{ButtonStyle, Colour, ComponentInteraction, CreateActionRow, CreateButton};
use std::path::PathBuf;
use tokio::sync::mpsc::Sender;

const BATCH_COMPONENT_PREFIX: &str = "pnbatch";

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

pub async fn handle_batch(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let Some(probe_job_id) = option_str(command, "job_id").and_then(|id| id.parse::<u64>().ok())
    else {
        command_error(ctx, command, "Error: job_id must be a number").await;
        return;
    };
    let Some(attachment) = option_attachment(command, "subtitles") else {
        command_error(ctx, command, "Error: a subtitle .zip is required").await;
        return;
    };
    let archive = match attachment.download().await {
        Ok(bytes) => bytes,
        Err(e) => {
            command_error(ctx, command, format!("Failed to download subtitles: {}", e)).await;
            return;
        }
    };

    let db = match JobDb::new().await {
        Ok(db) => db,
        Err(e) => {
            command_error(ctx, command, format!("Error: failed to open job DB: {}", e)).await;
            return;
        }
    };
    let (source, files) = match db.get_job(probe_job_id).await {
        Ok(Some(row)) => (row.link, probe_rows(row.progress.as_deref())),
        Ok(None) => {
            command_error(ctx, command, "Error: probe job was not found.").await;
            return;
        }
        Err(e) => {
            command_error(ctx, command, format!("Error: failed to read probe job: {}", e)).await;
            return;
        }
    };
    if files.is_empty() {
        command_error(ctx, command, "Error: that job has no probed file list.").await;
        return;
    }
    let selection = match select_files(&files, option_trimmed(command, "indexes").as_deref()) {
        Ok(selection) if !selection.is_empty() => selection,
        Ok(_) => {
            command_error(ctx, command, "Error: no file matched that index selection.").await;
            return;
        }
        Err(reason) => {
            command_error(ctx, command, reason).await;
            return;
        }
    };

    let subtitles = match read_subtitle_archive(&archive).await {
        Ok(subtitles) if !subtitles.is_empty() => subtitles,
        Ok(_) => {
            command_error(ctx, command, "Error: the archive holds no subtitle files.").await;
            return;
        }
        Err(reason) => {
            command_error(ctx, command, format!("Error: {}", reason)).await;
            return;
        }
    };

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
    let Some(response) = working_response(ctx, command, "...").await else {
        return;
    };

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
    if let Err(e) = write_pending(response.id.get(), &pending, &subtitles[..pairs]).await {
        command_error(ctx, command, format!("Error: {}", e)).await;
        let _ = response.delete(ctx).await;
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
    let mut response = response;
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

// `/probe` already renders its rows episode-sorted, so reading that stored list back keeps the
// pairing in the order the user was shown rather than the order the torrent packed its files.
fn probe_rows(progress: Option<&str>) -> Vec<(u64, String)> {
    let Some(progress) = progress else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(progress) else {
        return Vec::new();
    };
    if value.get("type").and_then(|value| value.as_str()) != Some("probe") {
        return Vec::new();
    }
    let Some(files) = value.get("files").and_then(|value| value.as_str()) else {
        return Vec::new();
    };
    files
        .lines()
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
