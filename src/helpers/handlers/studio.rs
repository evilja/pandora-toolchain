use super::*;
use pandora_toolchain::lib::image::timeline::{render_timeline, TimelineSpec, TimelineTrack};
use pandora_toolchain::lib::mpeg::studio::{PreviewWindow, StudioTrackMode, StudioVideoPreset};
use pandora_toolchain::pnworker::core::{KeepKind, Preset, StudioJobRequest};
use pandora_toolchain::pnworker::server_effects::load_server_settings;
use pandora_toolchain::pnworker::studio::{StudioMeta, StudioStore};
use serenity::builder::CreateAttachment;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc::Sender;

pub async fn handle_studio(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    tx: &Sender<JobClass>,
) {
    let Some((subcommand, _)) = subcommand_options(command) else {
        command_error(ctx, command, "Error: Studio subcommand is required.").await;
        return;
    };
    let Some(guild_id) = command_server_id(ctx, command, "/studio").await else {
        return;
    };
    let user_id = command.user.id.get();
    let store = StudioStore::new();

    match subcommand {
        "create" => {
            let Some(keywords) = required_trimmed_option(ctx, command, "keywords", "keywords").await else {
                return;
            };
            let Some(mut response) = working_response(ctx, command, "Creating Pandora Studio...").await else {
                return;
            };
            match store.create_from_keywords(guild_id, user_id, &keywords).await {
                Ok(meta) => edit_text(ctx, &mut response, studio_summary(&meta, "Pandora Studio created")).await,
                Err(e) => edit_text(ctx, &mut response, format!("Studio create failed: {}", e)).await,
            }
        }
        "keywords" => {
            let Some(keywords) = required_trimmed_option(ctx, command, "keywords", "keywords").await else {
                return;
            };
            let Some(mut response) = working_response(ctx, command, "Replacing Pandora Studio sources...").await else {
                return;
            };
            match store.replace_keywords(guild_id, user_id, &keywords).await {
                Ok((meta, removed_tracks)) => edit_text(ctx, &mut response, format!(
                    "Replaced Studio `{}` sources with {} keep(s). Duration: `{}`. Removed {} out-of-range track(s).",
                    meta.studio_id,
                    meta.sources.len(),
                    format_duration(meta.total_duration_ms),
                    removed_tracks,
                )).await,
                Err(e) => edit_text(ctx, &mut response, format!("Studio keyword replacement failed: {}", e)).await,
            }
        }
        "disown" => {
            let Some(mut response) = working_response(ctx, command, "Updating Pandora Studio...").await else {
                return;
            };
            match store.disown(guild_id, user_id).await {
                Ok(meta) => {
                    let state = if meta.collaborators.is_empty() {
                        "You disowned the Studio. It expires in 30 minutes unless reowned."
                    } else {
                        "You left the Studio. Other collaborators still own it."
                    };
                    edit_text(ctx, &mut response, format!("{}\nStudio ID: `{}`", state, meta.studio_id)).await;
                }
                Err(e) => edit_text(ctx, &mut response, format!("Studio disown failed: {}", e)).await,
            }
        }
        "reown" => {
            let requested = option_trimmed(command, "studio_id");
            let Some(mut response) = working_response(ctx, command, "Reowning Pandora Studio...").await else {
                return;
            };
            match store.reown(guild_id, user_id, requested.as_deref()).await {
                Ok(meta) => edit_text(ctx, &mut response, studio_summary(&meta, "Pandora Studio attached")).await,
                Err(e) => edit_text(ctx, &mut response, format!("Studio reown failed: {}", e)).await,
            }
        }
        "insert" | "override" | "duck" => {
            let Some(attachment) = option_attachment(command, "audio") else {
                command_error(ctx, command, "Error: an audio attachment is required.").await;
                return;
            };
            let mode = match subcommand {
                "insert" => StudioTrackMode::Insert,
                "override" => StudioTrackMode::Override,
                _ => StudioTrackMode::Duck,
            };
            let (duck_volume_percent, fade_ms) = if mode == StudioTrackMode::Duck {
                let Some(volume) = option_i64(command, "volume").filter(|value| (0..=100).contains(value)) else {
                    command_error(ctx, command, "Error: `volume` must be a percentage from 0 to 100.").await;
                    return;
                };
                let Some(fade) = option_f64(command, "fade").filter(|value| value.is_finite() && *value >= 0.0 && *value <= 3600.0) else {
                    command_error(ctx, command, "Error: `fade` must be from 0 to 3600 seconds.").await;
                    return;
                };
                (volume as u8, (fade * 1000.0).round() as u64)
            } else {
                (100, 0)
            };
            let Some(mut response) = working_response(ctx, command, "Adding Studio audio track...").await else {
                return;
            };
            if let Err(e) = store.inspect_current(guild_id, user_id).await {
                edit_text(ctx, &mut response, format!("Studio track upload failed: {}", e)).await;
                return;
            }
            let ext = safe_attachment_extension(&attachment.filename);
            let temp = std::env::temp_dir().join(format!(
                "pandora-studio-{}-{}.{}",
                user_id,
                response.id.get(),
                ext,
            ));
            let result = match attachment.download().await {
                Ok(bytes) => match tokio::fs::write(&temp, bytes).await {
                    Ok(()) => store
                        .add_track_from_path(
                            guild_id,
                            user_id,
                            &temp,
                            mode,
                            Some(&attachment.filename),
                            duck_volume_percent,
                            fade_ms,
                        )
                        .await,
                    Err(e) => Err(format!("failed to stage attachment: {}", e)),
                },
                Err(e) => Err(format!("failed to download attachment: {}", e)),
            };
            tokio::fs::remove_file(&temp).await.ok();
            match result {
                Ok(track) => {
                    let ducking = if track.mode == StudioTrackMode::Duck {
                        format!(
                            " Other audio target: `{}%`; fade: `{}` each way.",
                            track.duck_volume_percent,
                            format_duration_precise(track.fade_ms),
                        )
                    } else {
                        String::new()
                    };
                    edit_text(ctx, &mut response, format!(
                        "Added {:?} track `#{}` (`{}`), duration {}. Initial offset: `0:00`.{}",
                        track.mode,
                        track.id,
                        track.display_name,
                        format_duration(track.duration_ms),
                        ducking,
                    )).await;
                }
                Err(e) => edit_text(ctx, &mut response, format!("Studio track upload failed: {}", e)).await,
            }
        }
        "edittrack" => {
            let Some(track_id) = positive_track_option(ctx, command).await else {
                return;
            };
            let mode = match option_trimmed(command, "type").as_deref() {
                Some("insert") => Some(StudioTrackMode::Insert),
                Some("override") => Some(StudioTrackMode::Override),
                Some("duck") => Some(StudioTrackMode::Duck),
                Some(_) => {
                    command_error(ctx, command, "Error: `type` must be insert, override, or duck.").await;
                    return;
                }
                None => None,
            };
            let volume_percent = match option_i64(command, "volume") {
                Some(value) if (0..=200).contains(&value) => Some(value as u16),
                Some(_) => {
                    command_error(ctx, command, "Error: `volume` must be a percentage from 0 to 200.").await;
                    return;
                }
                None => None,
            };
            let duck_volume_percent = match option_i64(command, "duck_volume") {
                Some(value) if (0..=100).contains(&value) => Some(value as u8),
                Some(_) => {
                    command_error(ctx, command, "Error: `duck_volume` must be a percentage from 0 to 100.").await;
                    return;
                }
                None => None,
            };
            let fade_ms = match option_f64(command, "fade") {
                Some(value) if value.is_finite() && (0.0..=3600.0).contains(&value) => {
                    Some((value * 1000.0).round() as u64)
                }
                Some(_) => {
                    command_error(ctx, command, "Error: `fade` must be from 0 to 3600 seconds.").await;
                    return;
                }
                None => None,
            };
            if mode.is_none() && volume_percent.is_none() && duck_volume_percent.is_none() && fade_ms.is_none() {
                command_error(ctx, command, "Error: supply at least one track setting to edit.").await;
                return;
            }
            let Some(mut response) = working_response(ctx, command, "Editing Studio track...").await else {
                return;
            };
            match store.edit_track(
                guild_id,
                user_id,
                track_id,
                mode,
                volume_percent,
                duck_volume_percent,
                fade_ms,
            ).await {
                Ok(track) => {
                    let duck = if track.mode == StudioTrackMode::Duck {
                        format!(
                            " Duck target: `{}%`; fade: `{}` each way.",
                            track.duck_volume_percent,
                            format_duration_precise(track.fade_ms),
                        )
                    } else {
                        String::new()
                    };
                    edit_text(ctx, &mut response, format!(
                        "Edited track `#{}`. Type: `{:?}`; own volume: `{}%`.{}",
                        track.id,
                        track.mode,
                        track.volume_percent,
                        duck,
                    )).await;
                }
                Err(e) => edit_text(ctx, &mut response, format!("Studio track edit failed: {}", e)).await,
            }
        }
        "move" => {
            let Some(track_id) = positive_track_option(ctx, command).await else {
                return;
            };
            let Some(offset) = required_trimmed_option(ctx, command, "offset", "offset").await else {
                return;
            };
            let Some(mut response) = working_response(ctx, command, "Moving Studio track...").await else {
                return;
            };
            match store.move_track(guild_id, user_id, track_id, &offset).await {
                Ok(track) => edit_text(ctx, &mut response, format!(
                    "Moved track `#{}` to `{}`.", track.id, format_duration(track.offset_ms)
                )).await,
                Err(e) => edit_text(ctx, &mut response, format!("Studio move failed: {}", e)).await,
            }
        }
        "cut" => {
            let Some(track_id) = positive_track_option(ctx, command).await else {
                return;
            };
            let Some(side) = required_trimmed_option(ctx, command, "side", "side").await else {
                return;
            };
            let (cut_start, cut_end) = match side.as_str() {
                "start" => (true, false),
                "end" => (false, true),
                "both" => (true, true),
                _ => {
                    command_error(ctx, command, "Error: `side` must be start, end, or both.").await;
                    return;
                }
            };
            let Some(seconds) = option_f64(command, "seconds")
                .filter(|value| value.is_finite() && *value >= 0.001 && *value <= 86_400.0)
            else {
                command_error(ctx, command, "Error: `seconds` must be from 0.001 to 86400.").await;
                return;
            };
            let amount_ms = (seconds * 1000.0).round() as u64;
            let Some(mut response) = working_response(ctx, command, "Cutting Studio track...").await else {
                return;
            };
            match store.cut_track(guild_id, user_id, track_id, amount_ms, cut_start, cut_end).await {
                Ok(track) => edit_text(ctx, &mut response, format!(
                    "Cut track `#{}` by `{}` from {}. Remaining duration: `{}`. Total start/end cuts: `{}` / `{}`.",
                    track.id,
                    format_duration_precise(amount_ms),
                    side,
                    format_duration_precise(track.duration_ms),
                    format_duration_precise(track.trim_start_ms),
                    format_duration_precise(track.trim_end_ms),
                )).await,
                Err(e) => edit_text(ctx, &mut response, format!("Studio cut failed: {}", e)).await,
            }
        }
        "remove" => {
            let Some(track_id) = positive_track_option(ctx, command).await else {
                return;
            };
            let Some(mut response) = working_response(ctx, command, "Removing Studio track...").await else {
                return;
            };
            match store.remove_track(guild_id, user_id, track_id).await {
                Ok(meta) => edit_text(ctx, &mut response, format!(
                    "Removed track `#{}`. {} track(s) remain.", track_id, meta.tracks.len()
                )).await,
                Err(e) => edit_text(ctx, &mut response, format!("Studio remove failed: {}", e)).await,
            }
        }
        "timeline" => {
            let Some(mut response) = working_response(ctx, command, "Rendering Studio timeline...").await else {
                return;
            };
            match store.snapshot(guild_id, user_id).await {
                Ok(meta) => {
                    let spec = timeline_spec(&meta);
                    match tokio::task::spawn_blocking(move || render_timeline(&spec)).await {
                        Ok(Ok(png)) => {
                            let attachment = CreateAttachment::bytes(png, "pandora-studio-timeline.png");
                            let _ = response.edit(ctx, EditMessage::new()
                                .content(format!("Pandora Studio `{}` timeline", meta.studio_id))
                                .new_attachment(attachment)).await;
                        }
                        Ok(Err(e)) => edit_text(ctx, &mut response, format!("Timeline render failed: {}", e)).await,
                        Err(e) => edit_text(ctx, &mut response, format!("Timeline task failed: {}", e)).await,
                    }
                }
                Err(e) => edit_text(ctx, &mut response, format!("Studio timeline failed: {}", e)).await,
            }
        }
        "preview" => {
            let Some(track_id) = positive_track_option(ctx, command).await else {
                return;
            };
            let Some(response) = working_response(ctx, command, "Preparing Studio preview...").await else {
                return;
            };
            queue_studio_job(ctx, command, tx, store, guild_id, user_id, response, Some(track_id)).await;
        }
        "done" => {
            let Some(response) = working_response(ctx, command, "Preparing Studio output...").await else {
                return;
            };
            queue_studio_job(ctx, command, tx, store, guild_id, user_id, response, None).await;
        }
        other => command_error(ctx, command, format!("Unknown Studio subcommand `{}`.", other)).await,
    }
}

async fn queue_studio_job(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    tx: &Sender<JobClass>,
    store: StudioStore,
    guild_id: u64,
    user_id: u64,
    mut response: Message,
    preview_track: Option<u64>,
) {
    let preview = preview_track.is_some();
    let current = match store.inspect_current(guild_id, user_id).await {
        Ok(meta) => meta,
        Err(e) => {
            edit_text(ctx, &mut response, format!("Studio render failed: {}", e)).await;
            return;
        }
    };
    let (job_preset, video_preset) = if preview {
        (Preset::Dummy(None), StudioVideoPreset::Dummy)
    } else if current.source_kind == KeepKind::Encode {
        (Preset::Copy, StudioVideoPreset::Standard)
    } else {
        studio_server_preset(guild_id)
    };
    let job_id = response.id.get();
    let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        .join("DB").join("work").join(job_id.to_string());
    let (manifest, meta) = match store
        .stage_render_snapshot(guild_id, user_id, &directory, preview_track, video_preset)
        .await
    {
        Ok(snapshot) => snapshot,
        Err(e) => {
            edit_text(ctx, &mut response, format!("Studio render snapshot failed: {}", e)).await;
            return;
        }
    };

    response.react(ctx, '❌').await.ok();
    let job_type = if preview { JobType::StudioPreview } else { JobType::Studio };
    let mut job = Job::new(
        user_id,
        command.channel_id.get(),
        response.id.get(),
        job_type,
        job_id,
        TorrentType::Link(format!("studio:{}", meta.studio_id)),
        Vec::new(),
        ctx.clone(),
        response,
        read_lang(command.guild_id),
        Some(guild_id),
    );
    job.display_link = Some(studio_job_display(&meta, preview_track));
    job.preset = job_preset;
    job.studio = Some(StudioJobRequest { manifest });
    if tx.send(JobClass::Job(job)).await.is_err() {
        tokio::fs::remove_dir_all(directory).await.ok();
    }
}

fn studio_job_display(meta: &StudioMeta, preview_track: Option<u64>) -> String {
    let Some(track_id) = preview_track else {
        return format!("Pandora Studio `{}`", meta.studio_id);
    };
    let Some(track) = meta.tracks.iter().find(|track| track.id == track_id) else {
        return format!("Pandora Studio `{}`", meta.studio_id);
    };
    let window = PreviewWindow::around_track_start(track.offset_ms, meta.total_duration_ms);
    format!(
        "Pandora Studio `{}`\nTrack `#{}` offset: `{}`\nPreview window: `{}` - `{}`",
        meta.studio_id,
        track.id,
        format_timestamp(track.offset_ms),
        format_timestamp(window.start_ms),
        format_timestamp(window.start_ms.saturating_add(window.duration_ms)),
    )
}

fn studio_server_preset(guild_id: u64) -> (Preset, StudioVideoPreset) {
    match load_server_settings(Some(guild_id)).preset {
        Preset::PseudoLossless(_) => (Preset::PseudoLossless(None), StudioVideoPreset::PseudoLossless),
        Preset::Gpu(_) => (Preset::Gpu(None), StudioVideoPreset::Gpu),
        Preset::Dummy(_) => (Preset::Dummy(None), StudioVideoPreset::Dummy),
        Preset::Standard(_) | Preset::Copy => (Preset::Standard(None), StudioVideoPreset::Standard),
    }
}

async fn positive_track_option(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) -> Option<u64> {
    match option_i64(command, "track") {
        Some(value) if value > 0 => Some(value as u64),
        _ => {
            command_error(ctx, command, "Error: `track` must be a positive track number.").await;
            None
        }
    }
}

fn timeline_spec(meta: &StudioMeta) -> TimelineSpec {
    TimelineSpec {
        duration_ms: meta.total_duration_ms,
        tracks: meta.tracks.iter().map(|track| TimelineTrack {
            id: track.id,
            name: track.display_name.clone(),
            mode: track.mode,
            volume_percent: track.volume_percent,
            offset_ms: track.offset_ms,
            duration_ms: track.duration_ms,
        }).collect(),
    }
}

fn studio_summary(meta: &StudioMeta, heading: &str) -> String {
    format!(
        "{}\nStudio ID: `{}`\nInput: {} keep(s), {:?}\nDuration: `{}`\nCollaborators: {}\nExpires after 24 hours of inactivity.",
        heading,
        meta.studio_id,
        meta.sources.len(),
        meta.source_kind,
        format_duration(meta.total_duration_ms),
        meta.collaborators.len(),
    )
}

fn safe_attachment_extension(filename: &str) -> String {
    Path::new(filename).extension().and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|ext| ext.len() <= 8 && ext.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "audio".to_string())
}

fn format_duration(ms: u64) -> String {
    let total = ms / 1000;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{}:{:02}", minutes, seconds)
    }
}

fn format_duration_precise(ms: u64) -> String {
    if ms % 1000 == 0 {
        format!("{}s", ms / 1000)
    } else {
        format!("{:.3}s", ms as f64 / 1000.0)
    }
}

fn format_timestamp(ms: u64) -> String {
    let hours = ms / 3_600_000;
    let minutes = (ms % 3_600_000) / 60_000;
    let seconds = (ms % 60_000) / 1000;
    let millis = ms % 1000;
    if hours > 0 {
        format!("{}:{:02}:{:02}.{:03}", hours, minutes, seconds, millis)
    } else {
        format!("{}:{:02}.{:03}", minutes, seconds, millis)
    }
}

async fn edit_text(ctx: &Context, response: &mut Message, text: impl Into<String>) {
    let _ = response.edit(ctx, EditMessage::new().content(text.into())).await;
}
