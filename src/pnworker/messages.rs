use crate::pnworker::core::{Job, JobType, Stage};
use serde::{Deserialize, Serialize};
use serenity::all::{Colour, CreateEmbed, CreateEmbedFooter};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

const PKGVER: &str = env!("CARGO_PKG_VERSION");
const EN_LOCALE: &str = include_str!("locales/en.toml");
const TR_LOCALE: &str = include_str!("locales/tr.toml");
const JP_LOCALE: &str = include_str!("locales/jp.toml");
const LEGACY_LOCALE: &str = include_str!("locales/legacy.toml");

pub const QUEUE_TOO_LONG: &str = "QUEUE_TOO_LONG";
pub const QUEUED: &str = "QUEUED";
pub const JOB_SETUP_FAIL: &str = "JOB_SETUP_FAIL";
pub const JOB_CANCELLED: &str = "JOB_CANCELLED";
pub const PROBE_TIMEOUT: &str = "PROBE_TIMEOUT";
pub const GITSYNC_PROGRESS: &str = "GITSYNC_PROGRESS";
pub const GITSYNC_SUCCESS: &str = "GITSYNC_SUCCESS";
pub const GITSYNC_FAIL: &str = "GITSYNC_FAIL";
pub const GITQUERY_BLOCKED: &str = "GITQUERY_BLOCKED";
pub const CTORRENT_DONE: &str = "CTORRENT_DONE";
pub const CTORRENT_FAIL: &str = "CTORRENT_FAIL";
pub const TORRENT_PROG: &str = "TORRENT_PROG";
pub const TORRENT_PROG_SELECT: &str = "TORRENT_PROG_SELECT";
pub const TORRENT_DONE: &str = "TORRENT_DONE";
pub const TORRENT_FAIL: &str = "TORRENT_FAIL";
pub const TORRENT_DUPLICATE_WAIT: &str = "TORRENT_DUPLICATE_WAIT";
pub const ENCODE_PROG: &str = "ENCODE_PROG";
pub const ENCODE_CONCAT_PROG: &str = "ENCODE_CONCAT_PROG";
pub const ENCODE_START: &str = "ENCODE_START";
pub const ENCODE_WARNING: &str = "ENCODE_WARNING";
pub const SERVER_EFFECTS_FAIL: &str = "SERVER_EFFECTS_FAIL";
pub const ENCODE_DONE: &str = "ENCODE_DONE";
pub const ENCODE_FAIL: &str = "ENCODE_FAIL";
pub const UPLOAD_PROG: &str = "UPLOAD_PROG";
pub const UPLOAD_DONE: &str = "UPLOAD_DONE";
pub const UPLOAD_FAIL: &str = "UPLOAD_FAIL";
pub const UPLOAD_BACKUP_PROG: &str = "UPLOAD_BACKUP_PROG";
pub const BACKUPALL_PROG: &str = "BACKUPALL_PROG";
pub const KEEP_READY: &str = "KEEP_READY";
pub const KEEP_DONE: &str = "KEEP_DONE";
pub const KEEP_FAIL: &str = "KEEP_FAIL";
pub const KEYCODE_WAIT: &str = "KEYCODE_WAIT";
pub const KEYCODE_FAIL: &str = "KEYCODE_FAIL";
pub const PROBE_DONE: &str = "PROBE_DONE";
pub const PROBE_FAIL: &str = "PROBE_FAIL";
pub const PROBE_ROW: &str = "PROBE_ROW";
pub const PREVIEW_DONE: &str = "PREVIEW_DONE";
pub const PREVIEW_FAIL: &str = "PREVIEW_FAIL";
pub const STUDIO_PREVIEW_DONE: &str = "STUDIO_PREVIEW_DONE";
pub const STUDIO_PREVIEW_FAIL: &str = "STUDIO_PREVIEW_FAIL";
pub const PREVIEW_ATTACHMENT_REJECTED: &str = "PREVIEW_ATTACHMENT_REJECTED";
pub const PREVIEW_ATTACHMENT_MISSING: &str = "PREVIEW_ATTACHMENT_MISSING";
pub const STUDIO_PREVIEW_ATTACHMENT_MISSING: &str = "STUDIO_PREVIEW_ATTACHMENT_MISSING";
pub const EMBED_TITLE: &str = "EMBED_TITLE";
pub const EMBED_FOOTER: &str = "EMBED_FOOTER";
pub const FIELD_JOBID: &str = "FIELD_JOBID";
pub const FIELD_AUTHOR: &str = "FIELD_AUTHOR";
pub const FIELD_WORKER: &str = "FIELD_WORKER";
pub const FIELD_STATUS: &str = "FIELD_STATUS";
pub const FIELD_PRESET: &str = "FIELD_PRESET";
pub const FIELD_TORRENT: &str = "FIELD_TORRENT";
pub const FIELD_SOURCE: &str = "FIELD_SOURCE";
pub const FIELD_PROGRESS: &str = "FIELD_PROGRESS";
pub const FIELD_WARNINGS: &str = "FIELD_WARNINGS";
pub const FIELD_REPO: &str = "FIELD_REPO";
pub const FIELD_FILE: &str = "FIELD_FILE";
pub const FIELD_COMMIT: &str = "FIELD_COMMIT";
pub const FIELD_RELEASE: &str = "FIELD_RELEASE";
pub const FIELD_FONTS: &str = "FIELD_FONTS";
pub const FIELD_REQUESTED: &str = "FIELD_REQUESTED";
pub const FIELD_EPISODE: &str = "FIELD_EPISODE";
pub const FIELD_PATH: &str = "FIELD_PATH";
pub const FIELD_ANIME: &str = "FIELD_ANIME";
pub const FIELD_CHANNEL: &str = "FIELD_CHANNEL";
pub const FIELD_CREATED: &str = "FIELD_CREATED";
pub const FIELD_GLOBAL: &str = "FIELD_GLOBAL";
pub const FIELD_SERVER: &str = "FIELD_SERVER";
pub const FIELD_TOTAL: &str = "FIELD_TOTAL";
pub const FIELD_FILES: &str = "FIELD_FILES";
pub const FIELD_LOCATION: &str = "FIELD_LOCATION";
pub const FIELD_SLUG: &str = "FIELD_SLUG";
pub const FIELD_KIND: &str = "FIELD_KIND";
pub const FIELD_EPISODES: &str = "FIELD_EPISODES";
pub const FIELD_LANGUAGE: &str = "FIELD_LANGUAGE";
pub const FIELD_API_KEY: &str = "FIELD_API_KEY";
pub const FIELD_GDRIVE: &str = "FIELD_GDRIVE";
pub const FIELD_GDRIVE_ANONYMOUS: &str = "FIELD_GDRIVE_ANONYMOUS";
pub const FIELD_WRAPSTYLE: &str = "FIELD_WRAPSTYLE";
pub const FIELD_ANNOUNCEMENT: &str = "FIELD_ANNOUNCEMENT";
pub const FIELD_CONCAT: &str = "FIELD_CONCAT";
pub const FIELD_LOCAL_GDRIVE: &str = "FIELD_LOCAL_GDRIVE";
pub const FIELD_DRIVE_ONLY: &str = "FIELD_DRIVE_ONLY";
pub const FIELD_ANIMECIX_FANSUB: &str = "FIELD_ANIMECIX_FANSUB";
pub const FIELD_OPENANIME_FANSUB: &str = "FIELD_OPENANIME_FANSUB";
pub const FIELD_ANIZM_FANSUB: &str = "FIELD_ANIZM_FANSUB";
pub const LABEL_ETA: &str = "LABEL_ETA";
pub const WARNINGS_MORE: &str = "WARNINGS_MORE";
pub const STAGE_QUEUED: &str = "STAGE_QUEUED";
pub const STAGE_PROBING: &str = "STAGE_PROBING";
pub const STAGE_PROBED: &str = "STAGE_PROBED";
pub const STAGE_DOWNLOADING: &str = "STAGE_DOWNLOADING";
pub const STAGE_DOWNLOADED: &str = "STAGE_DOWNLOADED";
pub const STAGE_ENCODING: &str = "STAGE_ENCODING";
pub const STAGE_ENCODED: &str = "STAGE_ENCODED";
pub const STAGE_UPLOADING: &str = "STAGE_UPLOADING";
pub const STAGE_UPLOADED: &str = "STAGE_UPLOADED";
pub const STAGE_FAILED: &str = "STAGE_FAILED";
pub const STAGE_DECLINED: &str = "STAGE_DECLINED";
pub const STAGE_CANCELLED: &str = "STAGE_CANCELLED";
pub const PRESET_PSEUDOLOSSLESS_INTRO: &str = "PRESET_PSEUDOLOSSLESS_INTRO";
pub const PRESET_PSEUDOLOSSLESS_NOINTRO: &str = "PRESET_PSEUDOLOSSLESS_NOINTRO";
pub const PRESET_GPU_INTRO: &str = "PRESET_GPU_INTRO";
pub const PRESET_GPU_NOINTRO: &str = "PRESET_GPU_NOINTRO";
pub const PRESET_STANDARD_INTRO: &str = "PRESET_STANDARD_INTRO";
pub const PRESET_STANDARD_NOINTRO: &str = "PRESET_STANDARD_NOINTRO";
pub const PRESET_DUMMY: &str = "PRESET_DUMMY";
pub const PRESET_COPY: &str = "PRESET_COPY";
pub const JOB_TYPE_ENCODE: &str = "JOB_TYPE_ENCODE";
pub const JOB_TYPE_PANCODE: &str = "JOB_TYPE_PANCODE";
pub const JOB_TYPE_PROBE: &str = "JOB_TYPE_PROBE";
pub const JOB_TYPE_BACKUP: &str = "JOB_TYPE_BACKUP";
pub const JOB_TYPE_BACKUP_ALL: &str = "JOB_TYPE_BACKUP_ALL";
pub const JOB_TYPE_KEYCODE: &str = "JOB_TYPE_KEYCODE";
pub const JOB_TYPE_PREVIEW: &str = "JOB_TYPE_PREVIEW";
pub const JOB_TYPE_STUDIO: &str = "JOB_TYPE_STUDIO";
pub const JOB_TYPE_STUDIO_PREVIEW: &str = "JOB_TYPE_STUDIO_PREVIEW";
pub const JOB_TYPE_UNKNOWN: &str = "JOB_TYPE_UNKNOWN";
pub const VALUE_NONE: &str = "VALUE_NONE";
pub const VALUE_NOT_AVAILABLE: &str = "VALUE_NOT_AVAILABLE";
pub const VALUE_SET: &str = "VALUE_SET";
pub const VALUE_UNSET: &str = "VALUE_UNSET";
pub const VALUE_ENABLED: &str = "VALUE_ENABLED";
pub const VALUE_DISABLED: &str = "VALUE_DISABLED";
pub const VALUE_MAGNET_HIDDEN: &str = "VALUE_MAGNET_HIDDEN";
pub const SOURCE_PROBE_FILE: &str = "SOURCE_PROBE_FILE";
pub const SOURCE_PROBE: &str = "SOURCE_PROBE";
pub const SOURCE_KEYWORDS: &str = "SOURCE_KEYWORDS";
pub const COMMAND_WORKING: &str = "COMMAND_WORKING";
pub const COMMAND_JOB_COMPLETE: &str = "COMMAND_JOB_COMPLETE";
pub const COMMAND_MERGE_COMPLETE: &str = "COMMAND_MERGE_COMPLETE";
pub const COMMAND_RELEASE_COMPLETE: &str = "COMMAND_RELEASE_COMPLETE";
pub const COMMAND_SOURCE_UPDATED: &str = "COMMAND_SOURCE_UPDATED";
pub const COMMAND_FILE_READY: &str = "COMMAND_FILE_READY";
pub const COMMAND_CHANNEL_DETACHED: &str = "COMMAND_CHANNEL_DETACHED";
pub const COMMAND_REPO_DELETED: &str = "COMMAND_REPO_DELETED";
pub const COMMAND_REPO_ATTACHED: &str = "COMMAND_REPO_ATTACHED";
pub const COMMAND_SERVER_CONFIGURED: &str = "COMMAND_SERVER_CONFIGURED";
pub const COMMAND_SERVER_UPDATED: &str = "COMMAND_SERVER_UPDATED";
pub const COMMAND_FONT_CHECK: &str = "COMMAND_FONT_CHECK";
pub const COMMAND_LOGS_READY: &str = "COMMAND_LOGS_READY";
pub const COMMAND_UPDATED: &str = "COMMAND_UPDATED";
pub const COMMAND_LIST: &str = "COMMAND_LIST";
pub const COMMAND_REPO_PRESERVED: &str = "COMMAND_REPO_PRESERVED";
pub const LINK_DOWNLOAD: &str = "LINK_DOWNLOAD";
pub const CATLOGS_DESCRIPTION: &str = "CATLOGS_DESCRIPTION";
pub const CATLOGS_NO_LOGS: &str = "CATLOGS_NO_LOGS";
pub const CATLOGS_BUILD_FAIL: &str = "CATLOGS_BUILD_FAIL";
pub const CATLOGS_ACTIVE: &str = "CATLOGS_ACTIVE";
pub const CATLOGS_ARCHIVED: &str = "CATLOGS_ARCHIVED";
pub const WORKER_ASSIGN: &str = "WORKER_ASSIGN";
pub const QUEUE_POSITION: &str = "QUEUE_POSITION";

pub const DEFAULT_LANGS: &[&str] = &["en", "tr", "jp"];

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct MessageEntry {
    pub text: String,
    pub args: usize,
}

// Existing language files keep custom entries. Missing keys are merged in, while values that
// exactly match Pandora's old generated table are upgraded to the new per-language defaults.
pub fn init_language_files() {
    let legacy = parse_entries(LEGACY_LOCALE).unwrap_or_default();
    for lang in DEFAULT_LANGS {
        let path = format!("DB/config/{}.toml", lang);
        if let Some(parent) = Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if !Path::new(&path).exists() {
            let _ = std::fs::write(&path, locale_source(lang));
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                eprintln!("[messages] failed to read {}: {}", path, error);
                continue;
            }
        };
        let mut entries = match parse_entries(&content) {
            Some(entries) => entries,
            None => {
                eprintln!("[messages] refusing to update invalid language file {}", path);
                continue;
            }
        };
        let defaults = parse_entries(locale_source(lang)).unwrap_or_default();
        let mut changed = false;
        for (id, default) in defaults {
            match entries.get(&id) {
                None => {
                    entries.insert(id, default);
                    changed = true;
                }
                Some(existing) if legacy.get(&id) == Some(existing) && existing != &default => {
                    entries.insert(id, default);
                    changed = true;
                }
                _ => {}
            }
        }
        if changed {
            match toml::to_string_pretty(&entries) {
                Ok(content) => {
                    if let Err(error) = std::fs::write(&path, content) {
                        eprintln!("[messages] failed to update {}: {}", path, error);
                    }
                }
                Err(error) => eprintln!("[messages] failed to serialize {}: {}", path, error),
            }
        }
    }
}

pub fn get_message(id: &str, lang: &str) -> String {
    lookup(id, lang).map(|(text, _)| text).unwrap_or_default()
}

pub fn get_arg_count(id: &str, lang: &str) -> Option<usize> {
    lookup(id, lang).map(|(_, args)| args)
}

pub fn format_message(id: &str, lang: &str, args: &[String]) -> String {
    substitute(&get_message(id, lang), args)
}

struct LangTable {
    mtime: Option<std::time::SystemTime>,
    entries: BTreeMap<String, MessageEntry>,
}

fn lang_cache() -> &'static std::sync::Mutex<HashMap<String, LangTable>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, LangTable>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn builtin_cache() -> &'static HashMap<String, BTreeMap<String, MessageEntry>> {
    static CACHE: std::sync::OnceLock<HashMap<String, BTreeMap<String, MessageEntry>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        DEFAULT_LANGS
            .iter()
            .map(|lang| {
                (
                    (*lang).to_string(),
                    parse_entries(locale_source(lang)).unwrap_or_default(),
                )
            })
            .collect()
    })
}

fn parse_entries(content: &str) -> Option<BTreeMap<String, MessageEntry>> {
    toml::from_str(content).ok()
}

fn normalized_lang(lang: &str) -> String {
    let lang = lang.to_ascii_lowercase();
    if DEFAULT_LANGS.contains(&lang.as_str()) {
        lang
    } else {
        "en".to_string()
    }
}

fn locale_source(lang: &str) -> &'static str {
    match lang.to_ascii_lowercase().as_str() {
        "tr" => TR_LOCALE,
        "jp" => JP_LOCALE,
        _ => EN_LOCALE,
    }
}

fn lookup(id: &str, lang: &str) -> Option<(String, usize)> {
    let lang = normalized_lang(lang);
    let path = format!("DB/config/{}.toml", lang);
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

    let mut cache = lang_cache().lock().unwrap();
    let needs_reload = match cache.get(&lang) {
        Some(table) => table.mtime != mtime,
        None => true,
    };
    if needs_reload {
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| parse_entries(&content))
            .unwrap_or_default();
        cache.insert(lang.clone(), LangTable { mtime, entries });
    }
    if let Some(entry) = cache.get(&lang).and_then(|table| table.entries.get(id)) {
        return Some((entry.text.clone(), entry.args));
    }
    drop(cache);

    builtin_cache()
        .get(&lang)
        .and_then(|entries| entries.get(id))
        .map(|entry| (entry.text.clone(), entry.args))
}

#[derive(Clone, Debug)]
pub enum MessagePayload {
    Static(&'static str),
    Progress(&'static str, Vec<String>),
}

pub fn format_payload(payload: &MessagePayload, lang: &str) -> String {
    match payload {
        MessagePayload::Static(id) => get_message(id, lang),
        MessagePayload::Progress(id, args) => {
            if *id == UPLOAD_DONE {
                return format_completed_upload(args, lang);
            }
            if let Some(expected) = get_arg_count(id, lang) {
                if args.len() < expected {
                    eprintln!(
                        "[messages] arg count mismatch for {}: expected at least {}, got {}",
                        id,
                        expected,
                        args.len()
                    );
                }
            }
            format_message(id, lang, args)
        }
    }
}

fn format_completed_upload(args: &[String], lang: &str) -> String {
    let links = args
        .iter()
        .take(5)
        .map(|value| value.trim())
        .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
        .collect::<Vec<_>>()
        .join("\n");
    if links.is_empty() {
        get_message(UPLOAD_FAIL, lang)
    } else {
        links
    }
}

fn substitute(template: &str, args: &[String]) -> String {
    let mut result = template.to_string();
    for arg in args {
        if let Some(pos) = result.find("{}") {
            result.replace_range(pos..pos + 2, arg);
        }
    }
    result
}

// Job embeds use the stage as the single status label. The details area contains only metrics,
// links, warnings, and actionable context, so it does not repeat “encoding” or similar text.
pub fn create_job_embed(job: &Job, payload: &MessagePayload) -> CreateEmbed {
    let lang = &job.lang;
    let mut details = job_details(job, payload);
    if let Some(eta) = active_encode_eta_text(payload) {
        if !details.is_empty() {
            details.push('\n');
        }
        details.push_str(&format!("{} `{}`", get_message(LABEL_ETA, lang), eta));
    }

    let colour = stage_colour(job.ready);
    let title = get_job_type_text(job.job_type, lang);
    let footer = format_message(EMBED_FOOTER, lang, &[PKGVER.to_string()]);
    let mut embed = CreateEmbed::new()
        .title(title)
        .colour(colour)
        .field(
            get_message(FIELD_STATUS, lang),
            format!("{} {}", stage_icon(job.ready), get_stage_text(job.ready, lang)),
            true,
        )
        .field(
            get_message(FIELD_JOBID, lang),
            format!("`{}`", job.job_id),
            true,
        )
        .field(
            get_message(FIELD_WORKER, lang),
            format!("`{}`", job.worker),
            true,
        )
        .field(
            get_message(FIELD_SOURCE, lang),
            truncate_embed_value(&job_source(job, lang)),
            false,
        );

    if !job.encode_warnings.is_empty() {
        embed = embed.field(
            get_message(FIELD_WARNINGS, lang),
            warnings_field(&job.encode_warnings, lang),
            false,
        );
    }
    if !details.is_empty() {
        embed = embed.field(
            get_message(FIELD_PROGRESS, lang),
            truncate_embed_value(&details),
            false,
        );
    }
    embed
        .footer(CreateEmbedFooter::new(footer))
        .timestamp(serenity::model::Timestamp::now())
}

fn job_details(job: &Job, payload: &MessagePayload) -> String {
    if matches!(
        payload,
        MessagePayload::Static(id)
            if matches!(*id, QUEUED | JOB_CANCELLED | TORRENT_DONE | ENCODE_START | ENCODE_DONE)
    ) {
        return String::new();
    }
    let details = format_payload(payload, &job.lang).trim().to_string();
    if !matches!(payload, MessagePayload::Progress(id, _) if *id == ENCODE_PROG) {
        return details;
    }
    let stage = get_stage_text(job.ready, &job.lang);
    strip_redundant_encode_line(&details, &stage)
}

fn strip_redundant_encode_line(details: &str, stage: &str) -> String {
    let mut lines = details.lines();
    let Some(first) = lines.next() else {
        return details.to_string();
    };
    let stage = stage.to_ascii_lowercase();
    let first_lower = first.to_ascii_lowercase();
    let repeats_stage = !stage.trim().is_empty() && first_lower.contains(stage.trim());
    let narration_only = !first.chars().any(|character| character.is_ascii_digit());
    if repeats_stage || narration_only {
        lines.collect::<Vec<_>>().join("\n").trim().to_string()
    } else {
        details.to_string()
    }
}

fn stage_colour(stage: Stage) -> Colour {
    match stage {
        Stage::Queued => Colour::LIGHT_GREY,
        Stage::Probing | Stage::Downloading => Colour::BLUE,
        Stage::Probed | Stage::Downloaded => Colour::DARK_BLUE,
        Stage::Encoding => Colour::ORANGE,
        Stage::Encoded => Colour::DARK_ORANGE,
        Stage::Uploading => Colour::PURPLE,
        Stage::Uploaded => Colour::DARK_GREEN,
        Stage::Failed => Colour::RED,
        Stage::Declined => Colour::DARK_TEAL,
        Stage::Cancelled => Colour::DARK_GREY,
    }
}

fn stage_icon(stage: Stage) -> &'static str {
    match stage {
        Stage::Queued => "🕓",
        Stage::Probing => "🔎",
        Stage::Probed => "📋",
        Stage::Downloading => "⬇️",
        Stage::Downloaded => "📥",
        Stage::Encoding => "⚙️",
        Stage::Encoded => "🎞️",
        Stage::Uploading => "⬆️",
        Stage::Uploaded => "✅",
        Stage::Failed => "❌",
        Stage::Declined => "⛔",
        Stage::Cancelled => "🛑",
    }
}

fn job_source(job: &Job, lang: &str) -> String {
    if let Some(request) = &job.keycode {
        let keywords = request
            .keywords
            .iter()
            .map(|keyword| format!("`{}`", keyword))
            .collect::<Vec<_>>()
            .join(", ");
        return format_message(SOURCE_KEYWORDS, lang, &[keywords]);
    }
    if let Some(display) = job.display_link.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return crate::lib::p2p::nyaaise::display_source_link(display);
    }
    if let Some(probe_job_id) = job.probe_job_id {
        return match job.probe_file_index {
            Some(index) => format_message(
                SOURCE_PROBE_FILE,
                lang,
                &[probe_job_id.to_string(), index.to_string()],
            ),
            None => format_message(SOURCE_PROBE, lang, &[probe_job_id.to_string()]),
        };
    }

    let source = match &job.torrent {
        crate::lib::p2p::nyaaise::TorrentType::Magnet(_) => {
            get_message(VALUE_MAGNET_HIDDEN, lang)
        }
        crate::lib::p2p::nyaaise::TorrentType::Link(link)
        | crate::lib::p2p::nyaaise::TorrentType::GDrive(link)
        | crate::lib::p2p::nyaaise::TorrentType::Direct(link) => {
            crate::lib::p2p::nyaaise::display_source_link(link)
        }
    };
    if source.trim().is_empty() {
        get_message(VALUE_NOT_AVAILABLE, lang)
    } else {
        source
    }
}

fn truncate_embed_value(value: &str) -> String {
    const LIMIT: usize = 1024;
    if value.chars().count() <= LIMIT {
        return value.to_string();
    }
    value.chars().take(LIMIT - 1).collect::<String>() + "…"
}

fn active_encode_eta_text(payload: &MessagePayload) -> Option<String> {
    let MessagePayload::Progress(id, args) = payload else {
        return None;
    };
    let (frame, total, fps) = if *id == ENCODE_PROG {
        (args.get(1)?, args.get(2)?, args.get(3)?)
    } else if *id == ENCODE_CONCAT_PROG {
        (args.first()?, args.get(1)?, args.get(2)?)
    } else {
        return None;
    };
    let frame = frame.parse::<u64>().ok()?;
    let total = total.parse::<u64>().ok()?;
    let fps = fps.parse::<f64>().ok()?;
    if fps <= 0.0 || total <= frame {
        return None;
    }
    Some(format_eta(((total - frame) as f64 / fps).ceil() as u64))
}

fn format_eta(secs: u64) -> String {
    let mins = secs.saturating_add(59) / 60;
    if mins < 60 {
        return format!("{}m", mins);
    }
    format!("{}h {:02}m", mins / 60, mins % 60)
}

fn warnings_field(warnings: &[String], lang: &str) -> String {
    let mut out = String::new();
    let mut hidden = 0usize;
    for warning in warnings {
        let next = if out.is_empty() {
            format!("• {}", warning)
        } else {
            format!("\n• {}", warning)
        };
        if out.len() + next.len() > 980 {
            hidden += 1;
        } else {
            out.push_str(&next);
        }
    }
    if hidden > 0 {
        let tail = format_message(WARNINGS_MORE, lang, &[hidden.to_string()]);
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&tail);
    }
    if out.is_empty() {
        get_message(VALUE_NONE, lang)
    } else {
        out
    }
}

pub fn get_job_type_text(job_type: JobType, lang: &str) -> String {
    let id = match job_type {
        JobType::Encode => JOB_TYPE_ENCODE,
        JobType::Pancode => JOB_TYPE_PANCODE,
        JobType::Probe => JOB_TYPE_PROBE,
        JobType::Backup => JOB_TYPE_BACKUP,
        JobType::BackupAll => JOB_TYPE_BACKUP_ALL,
        JobType::Keycode => JOB_TYPE_KEYCODE,
        JobType::Preview => JOB_TYPE_PREVIEW,
        JobType::Studio => JOB_TYPE_STUDIO,
        JobType::StudioPreview => JOB_TYPE_STUDIO_PREVIEW,
        _ => JOB_TYPE_UNKNOWN,
    };
    get_message(id, lang)
}

pub fn get_stage_text(stage: Stage, lang: &str) -> String {
    let id = match stage {
        Stage::Queued => STAGE_QUEUED,
        Stage::Probing => STAGE_PROBING,
        Stage::Probed => STAGE_PROBED,
        Stage::Downloading => STAGE_DOWNLOADING,
        Stage::Downloaded => STAGE_DOWNLOADED,
        Stage::Encoding => STAGE_ENCODING,
        Stage::Encoded => STAGE_ENCODED,
        Stage::Uploading => STAGE_UPLOADING,
        Stage::Uploaded => STAGE_UPLOADED,
        Stage::Failed => STAGE_FAILED,
        Stage::Declined => STAGE_DECLINED,
        Stage::Cancelled => STAGE_CANCELLED,
    };
    get_message(id, lang)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::p2p::nyaaise::TorrentType;
    use crate::pnworker::core::Preset;
    use crate::pnworker::frontend::Frontend;
    use std::path::PathBuf;
    use std::time::Duration;

    fn test_job(job_type: JobType, source: &str) -> Job {
        Job {
            author: 1,
            channel_id: 2,
            response_id: 3,
            requested_at: Duration::from_secs(1),
            job_type,
            job_id: 4,
            preset: Preset::Standard(Some("intro".to_string())),
            torrent: TorrentType::Link(source.to_string()),
            display_link: None,
            attachment: Vec::new(),
            server_watermark: None,
            frontend: Frontend::None,
            directory: PathBuf::from("DB/work/4"),
            ready: Stage::Queued,
            probe_files: None,
            probe_torrent_path: None,
            probe_job_id: None,
            probe_file_index: None,
            lang: "en".to_string(),
            server_id: Some(5),
            acix: None,
            gdrive_folder_global: None,
            gdrive_folder_local: None,
            smartcode_drive_name: None,
            worker: "que-main".to_string(),
            duplicate_source: None,
            forward_parent: None,
            encode_warnings: Vec::new(),
            encode_dispatched: false,
            encode_dispatch_order: None,
            encode_frame: None,
            encode_total: None,
            encode_fps: None,
            keep: None,
            keycode: None,
            preview: None,
            studio: None,
        }
    }

    #[test]
    fn built_in_locales_have_matching_keys_and_arg_counts() {
        let en = parse_entries(EN_LOCALE).unwrap();
        let tr = parse_entries(TR_LOCALE).unwrap();
        let jp = parse_entries(JP_LOCALE).unwrap();
        assert_eq!(en.keys().collect::<Vec<_>>(), tr.keys().collect::<Vec<_>>());
        assert_eq!(en.keys().collect::<Vec<_>>(), jp.keys().collect::<Vec<_>>());
        for (id, entry) in en {
            assert_eq!(Some(entry.args), tr.get(&id).map(|value| value.args), "{}", id);
            assert_eq!(Some(entry.args), jp.get(&id).map(|value| value.args), "{}", id);
        }
    }

    #[test]
    fn job_types_have_distinct_localized_titles() {
        assert_ne!(get_job_type_text(JobType::Encode, "en"), get_job_type_text(JobType::Probe, "en"));
        assert_ne!(get_job_type_text(JobType::Backup, "tr"), get_job_type_text(JobType::Preview, "tr"));
        assert_ne!(get_job_type_text(JobType::Studio, "jp"), get_job_type_text(JobType::StudioPreview, "jp"));
    }

    #[test]
    fn empty_status_payloads_do_not_create_details_text() {
        assert!(format_payload(&MessagePayload::Static(ENCODE_START), "en").is_empty());
        assert!(format_payload(&MessagePayload::Static(TORRENT_DONE), "tr").is_empty());
        assert!(format_payload(&MessagePayload::Static(ENCODE_DONE), "jp").is_empty());
    }

    #[test]
    fn completed_upload_only_renders_successful_links() {
        let payload = MessagePayload::Progress(
            UPLOAD_DONE,
            vec![
                "https://drive.example/file".to_string(),
                "Doodstream Başarısız".to_string(),
                "https://lulu.example/e/file".to_string(),
                "Voe Başarısız".to_string(),
                "Abyss Başarısız".to_string(),
            ],
        );
        assert_eq!(
            format_payload(&payload, "en"),
            "https://drive.example/file\nhttps://lulu.example/e/file",
        );
    }

    #[test]
    fn completed_upload_without_links_renders_upload_failure() {
        let payload = MessagePayload::Progress(
            UPLOAD_DONE,
            vec!["Google Başarısız".to_string(), "Voe Başarısız".to_string()],
        );
        assert_eq!(format_payload(&payload, "en"), get_message(UPLOAD_FAIL, "en"));
    }

    #[test]
    fn job_embed_uses_real_type_view_link_and_no_preset() {
        let job = test_job(JobType::Probe, "https://nyaa.si/download/123.torrent");
        let embed = serde_json::to_value(create_job_embed(
            &job,
            &MessagePayload::Static(QUEUED),
        ))
        .unwrap();
        assert_eq!(
            embed.get("title").and_then(|value| value.as_str()),
            Some(get_job_type_text(JobType::Probe, "en").as_str())
        );
        let fields = embed.get("fields").and_then(|value| value.as_array()).unwrap();
        assert!(!fields.iter().any(|field| {
            field.get("name").and_then(|value| value.as_str())
                == Some(get_message(FIELD_PRESET, "en").as_str())
        }));
        let source = fields
            .iter()
            .find(|field| {
                field.get("name").and_then(|value| value.as_str())
                    == Some(get_message(FIELD_SOURCE, "en").as_str())
            })
            .and_then(|field| field.get("value"))
            .and_then(|value| value.as_str());
        assert_eq!(source, Some("https://nyaa.si/view/123"));
        assert!(!fields.iter().any(|field| {
            field.get("name").and_then(|value| value.as_str())
                == Some(get_message(FIELD_PROGRESS, "en").as_str())
        }));
    }

    #[test]
    fn job_embed_never_has_a_blank_source() {
        let job = test_job(JobType::Backup, "");
        let embed = serde_json::to_value(create_job_embed(
            &job,
            &MessagePayload::Static(QUEUED),
        ))
        .unwrap();
        let source = embed
            .get("fields")
            .and_then(|value| value.as_array())
            .unwrap()
            .iter()
            .find(|field| {
                field.get("name").and_then(|value| value.as_str())
                    == Some(get_message(FIELD_SOURCE, "en").as_str())
            })
            .and_then(|field| field.get("value"))
            .and_then(|value| value.as_str())
            .unwrap();
        assert!(!source.trim().is_empty());
    }

    #[test]
    fn legacy_encode_narration_is_removed_but_metrics_are_kept() {
        assert_eq!(
            strip_redundant_encode_line(
                "Dosya encode ediliyor.\nAşama: 1/2\nİşlenen kare: 40/100",
                "Encode ediliyor",
            ),
            "Aşama: 1/2\nİşlenen kare: 40/100"
        );
        assert_eq!(
            strip_redundant_encode_line(
                "Pass `1/2`\nFrames `40 / 100` • `20 FPS`",
                "Encoding",
            ),
            "Pass `1/2`\nFrames `40 / 100` • `20 FPS`"
        );
    }
}
