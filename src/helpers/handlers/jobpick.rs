use super::*;

use pandora_toolchain::lib::db::core::{stage_from_int, JobRow};
use serenity::builder::CreateAutocompleteResponse;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// The `job` option of every command that acts on a finished job. It used to be `job_id`, a
// snowflake copied off the job's message; now it is picked from a list of the channel's jobs, or
// left out, in which case the newest one is meant. What travels underneath is still the job id —
// the option's value — so a pasted id keeps working.
pub const JOB_OPTION: &str = "job";

// Discord shows at most this many choices, each at most `CHOICE_LABEL_CHARS` long.
const MAX_CHOICES: usize = 25;
const CHOICE_LABEL_CHARS: usize = 100;
// How far back the list reaches before it is narrowed by what was typed.
const CANDIDATE_ROWS: i64 = 100;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum JobScope {
    // Jobs with uploaded links: what there is to publish.
    Uploaded,
    // Anything the channel queued, however it ended: what there are logs of.
    Any,
}

fn scope_of(command_name: &str) -> JobScope {
    if command_name == "catlogs" {
        JobScope::Any
    } else {
        JobScope::Uploaded
    }
}

async fn candidate_rows(channel_id: u64, scope: JobScope) -> Vec<JobRow> {
    let Ok(db) = JobDb::new().await else {
        return Vec::new();
    };
    match scope {
        JobScope::Uploaded => db.get_uploaded_jobs_by_channel(channel_id).await,
        JobScope::Any => db.get_recent_jobs_by_channel(channel_id, CANDIDATE_ROWS).await,
    }
    .unwrap_or_default()
}

// What a job is recognised by: the episode when one was recorded, the name its file had in the
// torrent (or the link, for a job from before names were kept), how long ago it was asked for,
// and — where unfinished jobs are listed too — how it ended.
fn choice_label(row: &JobRow, scope: JobScope, now: u64, lang: &str) -> String {
    let mut tail = format!(" · {}", age_text(now.saturating_sub(row.requested_at.max(0) as u64)));
    if scope == JobScope::Any {
        if let Some(stage) = stage_from_int(row.stage) {
            tail.push_str(&format!(" · {}", get_stage_text(stage, lang)));
        }
    }
    let head = match row.episode {
        Some(episode) => format!("E{:02} · ", episode),
        None => String::new(),
    };
    let name = row
        .source_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| display_source_link(&row.link));
    let room = CHOICE_LABEL_CHARS.saturating_sub(head.chars().count() + tail.chars().count());
    format!("{}{}{}", head, clip(&name, room), tail)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max.saturating_sub(1)).collect::<String>())
}

fn age_text(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{}s", seconds),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

// Whether a row survives what was typed: a fragment of its label, or of its id for somebody who
// still has one to paste.
fn matches_partial(label: &str, job_id: i64, partial: &str) -> bool {
    let partial = partial.trim().to_lowercase();
    partial.is_empty()
        || label.to_lowercase().contains(&partial)
        || job_id.to_string().contains(&partial)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

// Answers an autocomplete on the `job` option, for whichever command carries it. Returns whether
// it did, so the caller goes on to the command's own autocompletes when the focus is elsewhere.
pub async fn handle_job_autocomplete(
    ctx: &Context,
    interaction: &serenity::all::CommandInteraction,
) -> bool {
    let Some(partial) = interaction
        .data
        .autocomplete()
        .filter(|option| option.name == JOB_OPTION)
        .map(|option| option.value.to_string())
    else {
        return false;
    };
    let scope = scope_of(interaction.data.name.as_str());
    let lang = read_lang(interaction.guild_id);
    let now = unix_now();
    let mut response = CreateAutocompleteResponse::new();
    let rows = candidate_rows(interaction.channel_id.get(), scope).await;
    for (label, job_id) in rows
        .iter()
        .map(|row| (choice_label(row, scope, now, &lang), row.job_id))
        .filter(|(label, job_id)| matches_partial(label, *job_id, &partial))
        .take(MAX_CHOICES)
    {
        response = response.add_string_choice(label, job_id.to_string());
    }
    interaction
        .create_response(ctx, CreateInteractionResponse::Autocomplete(response))
        .await
        .ok();
    true
}

// The job a command is about. With the option filled in it is that job; left empty it is the
// newest one in the channel when `default_latest` allows it — and the reply then says which job
// that turned out to be (`with_job_note`), because acting on a guess nobody can see is how the
// wrong episode gets published. `None` means the person has already been told why.
pub async fn resolve_job_option(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    default_latest: bool,
) -> Option<u64> {
    if let Some(raw) = option_trimmed(command, JOB_OPTION) {
        return match raw.parse::<u64>() {
            Ok(job_id) => Some(job_id),
            Err(_) => {
                command_error(ctx, command, command_format(command, JOB_PICK_INVALID, &[])).await;
                None
            }
        };
    }
    if !default_latest {
        command_error(ctx, command, command_format(command, JOB_PICK_REQUIRED, &[])).await;
        return None;
    }
    let scope = scope_of(command.data.name.as_str());
    let Some(row) = candidate_rows(command.channel_id.get(), scope).await.into_iter().next() else {
        command_error(ctx, command, command_format(command, JOB_PICK_NONE, &[])).await;
        return None;
    };
    let label = choice_label(&row, scope, unix_now(), &command_language(command));
    let mut notes = job_notes().lock().unwrap();
    // Notes are only ever read while their command is replying; a full map is old ones.
    if notes.len() >= 256 {
        notes.clear();
    }
    notes.insert(command.id.get(), command_format(command, JOB_PICK_DEFAULTED, &[label]));
    Some(row.job_id as u64)
}

fn job_notes() -> &'static Mutex<HashMap<u64, String>> {
    static NOTES: OnceLock<Mutex<HashMap<u64, String>>> = OnceLock::new();
    NOTES.get_or_init(|| Mutex::new(HashMap::new()))
}

// A command's reply, headed by the job it was run against when that job was assumed rather than
// named. Keyed by the interaction so the reply funnels can call it without being handed anything.
pub fn with_job_note(command: &serenity::all::CommandInteraction, content: String) -> String {
    match job_notes().lock().unwrap().get(&command.id.get()) {
        Some(note) => format!("{}\n{}", note, content),
        None => content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(episode: Option<i64>, source_name: Option<&str>, link: &str) -> JobRow {
        JobRow {
            job_id: 1234567890,
            author: 1,
            channel_id: 2,
            response_id: 3,
            requested_at: 1_000,
            started_at: None,
            ended_at: None,
            cancel_reason: None,
            job_type: 1,
            preset_type: 1,
            preset_name: None,
            candidates: None,
            outro: None,
            link: link.to_string(),
            directory: String::new(),
            stage: 7,
            archived: 1,
            progress: None,
            uploaded_links: None,
            acix_pending: None,
            server_id: None,
            episode,
            source_name: source_name.map(str::to_string),
            worker: "que-main".to_string(),
        }
    }

    #[test]
    fn a_job_is_labelled_by_episode_name_and_age() {
        let job = row(Some(5), Some("[Group] Show - 05.mkv"), "https://nyaa.si/view/1");
        assert_eq!(
            choice_label(&job, JobScope::Uploaded, 1_000 + 7_200, "en"),
            "E05 · [Group] Show - 05.mkv · 2h"
        );
    }

    #[test]
    fn a_job_from_before_names_were_kept_falls_back_to_its_link() {
        let job = row(None, None, "https://nyaa.si/view/42");
        assert_eq!(
            choice_label(&job, JobScope::Uploaded, 1_030, "en"),
            "https://nyaa.si/view/42 · 30s"
        );
    }

    #[test]
    fn a_label_never_outgrows_what_discord_accepts_and_keeps_its_tail() {
        let job = row(Some(1), Some(&"x".repeat(300)), "");
        let label = choice_label(&job, JobScope::Any, 1_000 + 90_000, "en");
        assert_eq!(label.chars().count(), CHOICE_LABEL_CHARS);
        assert!(label.starts_with("E01 · "));
        assert!(label.contains(" · 1d · "));
    }

    #[test]
    fn typing_narrows_by_name_or_by_id() {
        assert!(matches_partial("E05 · Show - 05.mkv · 2h", 1234567890, ""));
        assert!(matches_partial("E05 · Show - 05.mkv · 2h", 1234567890, "show - 05"));
        assert!(matches_partial("E05 · Show - 05.mkv · 2h", 1234567890, "34567"));
        assert!(!matches_partial("E05 · Show - 05.mkv · 2h", 1234567890, "Other"));
    }
}
