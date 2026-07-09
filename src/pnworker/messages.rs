use crate::pnworker::core::{Job, Preset, Stage};
use serde::{Deserialize, Serialize};
use serenity::all::{Colour, CreateEmbed};
use std::collections::HashMap;
use std::path::Path;

const PKGVER: &'static str = env!("CARGO_PKG_VERSION");

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
pub const EMBED_TITLE: &str = "EMBED_TITLE";
pub const FIELD_JOBID: &str = "FIELD_JOBID";
pub const FIELD_AUTHOR: &str = "FIELD_AUTHOR";
pub const FIELD_WORKER: &str = "FIELD_WORKER";
pub const FIELD_STATUS: &str = "FIELD_STATUS";
pub const FIELD_PRESET: &str = "FIELD_PRESET";
pub const FIELD_TORRENT: &str = "FIELD_TORRENT";
pub const FIELD_PROGRESS: &str = "FIELD_PROGRESS";
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
pub const WORKER_ASSIGN: &str = "WORKER_ASSIGN";
pub const QUEUE_POSITION: &str = "QUEUE_POSITION";

pub const DEFAULT_LANGS: &[&str] = &["en", "tr", "jp"];

static DEFAULT_ENTRIES: &[(&str, &str, usize)] = &[
    (
        "QUEUE_TOO_LONG",
        "\n\nŞu anda Pandora Toolchain'de biraz sıra var. \nLütfen daha sonra tekrar deneyin.",
        0,
    ),
    ("QUEUED", "\n\nİsteğiniz alındı.", 0),
    ("JOB_SETUP_FAIL", "\n\nİşlem hazırlanamadı: {}", 1),
    ("JOB_CANCELLED", "\nİşlem iptal edildi.", 0),
    (
        "PROBE_TIMEOUT",
        "Probe timed out. use /pancode or /backup within 3 minutes next time.",
        0,
    ),
    ("GITSYNC_PROGRESS", "Tüm işlemler kapatılıyor.", 0),
    (
        "GITSYNC_SUCCESS",
        "Kaynak kodlar git ile güncellendi.\nBot yeniden başlatılıyor.",
        0,
    ),
    (
        "GITSYNC_FAIL",
        "Git güncellemesi başarısız oldu.\nBot yine de yeniden başlatılıyor.",
        0,
    ),
    (
        "GITQUERY_BLOCKED",
        "Git query mevcut encode işlemlerinin bitmesini bekliyor. Git sync tamamlanana kadar yeni encode işleri kapalı.",
        0,
    ),
    (
        "CTORRENT_DONE",
        "\n\nTorrent kısa süre içinde indirilmeye başlanacak.",
        0,
    ),
    ("CTORRENT_FAIL", "\n\nTorrent metadatası indirilemedi.", 0),
    ("TORRENT_PROG", "\n\nTorrent ilerlemesi: {}% {}MB/{}MB", 3),
    ("TORRENT_PROG_SELECT", "\n\nTorrent ilerlemesi: {}% {}MB", 2),
    ("TORRENT_DONE", "\n\nEncode kısa süre içinde başlayacak.", 0),
    ("TORRENT_FAIL", "\n\nTorrent indirilemedi.", 0),
    (
        "TORRENT_DUPLICATE_WAIT",
        "\n\nİstediğiniz dosya mevcut olduğundan indirilmeyecek. \nDosyayı kullanan işlemin bitmesi bekleniyor.",
        1,
    ),
    (
        "ENCODE_PROG",
        "\n\nDosya encode ediliyor.\nAşama: 1/{}\nİşlenen kare: {}/{}\nSaniye başına işlenen kare: {}\nSaniye başına ortalama veri: {}kbit/s",
        5,
    ),
    ("ENCODE_START", "\n\nDosya encode ediliyor.", 0),
    (
        "ENCODE_CONCAT_PROG",
        "\n\nDosyaya intro ekleniyor.\nAşama: 2/2\nİşlenen kare: {}/{}\nSaniye başına işlenen kare: {}\nSaniye başına ortalama veri: {}kbit/s",
        4,
    ),
    (
        "QUEUE_POSITION",
        "\n\nİşlem encode sırasında #{} konumunda.\nTahmini bekleme: {}",
        2,
    ),
    ("ENCODE_DONE", "\n\nÇıktı sunuculara yükleniyor.", 0),
    ("ENCODE_FAIL", "\n\nDosya encode edilemedi.", 0),
    (
        "KEEP_READY",
        "\n\nKeep keyword ayrıldı.\nParent keyword: `{}`\nKeyword: `{}`",
        2,
    ),
    (
        "KEEP_DONE",
        "\n\nÇıktı 5 saatliğine saklandı.\nKeyword: `{}`\nParent keyword: `{}`\nTip: {}",
        3,
    ),
    ("KEEP_FAIL", "\n\nKeep işlemi başarısız oldu: {}", 1),
    ("KEYCODE_WAIT", "\n\nKeycode keyword bekliyor: {}", 1),
    ("KEYCODE_FAIL", "\n\nKeycode işlemi başarısız oldu: {}", 1),
    (
        "UPLOAD_PROG",
        "\n\nYükleme ilerlemesi:\n{}\n{}\n{}\n{}\n{}",
        5,
    ),
    ("UPLOAD_DONE", "\n\nDosya yüklendi.\n{}\n{}\n{}\n{}\n{}", 5),
    (
        "UPLOAD_FAIL",
        "\n\nDosya yüklenemedi. \nBir yetkiliden botu yeniden başlatmasını isteyebilirsiniz.",
        0,
    ),
    ("UPLOAD_BACKUP_PROG", "\n\n{}", 1),
    ("BACKUPALL_PROG", "\n\n{}", 1),
    ("PROBE_DONE", "\n\nBatch torrent incelendi.", 0),
    ("PROBE_FAIL", "\n\nBatch torrent incelenemedi.", 0),
    ("PROBE_ROW", "\n\nDosya numaraları:\n{}", 1),
    ("PREVIEW_DONE", "\n\nÖnizleme hazır: {} kare.", 1),
    ("PREVIEW_FAIL", "\n\nÖnizleme oluşturulamadı: {}", 1),
    ("EMBED_TITLE", "Encode İşlemi ({})", 1),
    ("FIELD_JOBID", "İşlem Numarası", 0),
    ("FIELD_AUTHOR", "İşlem Sahibi", 0),
    ("FIELD_WORKER", "Worker", 0),
    ("FIELD_STATUS", "Durum", 0),
    ("FIELD_PRESET", "Encode Preset", 0),
    ("FIELD_TORRENT", "Torrent Linki", 0),
    ("FIELD_PROGRESS", "İlerleme", 0),
    ("STAGE_QUEUED", "Sırada", 0),
    ("STAGE_PROBING", "İnceleniyor", 0),
    ("STAGE_PROBED", "İncelendi", 0),
    ("STAGE_DOWNLOADING", "İndiriliyor", 0),
    ("STAGE_DOWNLOADED", "İndirildi", 0),
    ("STAGE_ENCODING", "Encode Ediliyor", 0),
    ("STAGE_ENCODED", "Encode Edildi", 0),
    ("STAGE_UPLOADING", "Yükleniyor", 0),
    ("STAGE_UPLOADED", "Tamamlandı", 0),
    ("STAGE_FAILED", "Başarısız", 0),
    ("STAGE_DECLINED", "Reddedildi", 0),
    ("STAGE_CANCELLED", "İptal Edildi", 0),
    (
        "PRESET_PSEUDOLOSSLESS_INTRO",
        "Kayıpsız - İşlemci | İntrolu",
        0,
    ),
    (
        "PRESET_PSEUDOLOSSLESS_NOINTRO",
        "Kayıpsız - İşlemci | İntrosuz",
        0,
    ),
    ("PRESET_GPU_INTRO", "Standart - Ekran kartı | İntrolu", 0),
    ("PRESET_GPU_NOINTRO", "Standart - Ekran kartı | İntrosuz", 0),
    ("PRESET_STANDARD_INTRO", "Standart - İşlemci | İntrolu", 0),
    (
        "PRESET_STANDARD_NOINTRO",
        "Standart - İşlemci | İntrosuz",
        0,
    ),
    ("PRESET_DUMMY", "DEVELOPER", 0),
];

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct MessageEntry {
    pub text: String,
    pub args: usize,
}

pub fn init_language_files() {
    for lang in DEFAULT_LANGS {
        let path = format!("DB/config/{}.toml", lang);
        if Path::new(&path).exists() {
            continue;
        }
        if let Some(parent) = Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut body = String::new();
        for (id, text, args) in DEFAULT_ENTRIES {
            body.push_str(&format!("[{}]\ntext = {:?}\nargs = {}\n\n", id, text, args));
        }
        let _ = std::fs::write(&path, body);
    }
}

pub fn get_message(id: &str, lang: &str) -> String {
    lookup(id, lang).map(|(text, _)| text).unwrap_or_default()
}

pub fn get_arg_count(id: &str, lang: &str) -> Option<usize> {
    lookup(id, lang).map(|(_, args)| args)
}

struct LangTable {
    mtime: Option<std::time::SystemTime>,
    entries: HashMap<String, MessageEntry>,
}

fn lang_cache() -> &'static std::sync::Mutex<HashMap<String, LangTable>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, LangTable>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn lookup(id: &str, lang: &str) -> Option<(String, usize)> {
    let lang = lang.to_ascii_lowercase();
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
            .and_then(|content| toml::from_str::<HashMap<String, MessageEntry>>(&content).ok())
            .unwrap_or_default();
        cache.insert(lang.clone(), LangTable { mtime, entries });
    }
    if let Some(entry) = cache.get(&lang).and_then(|t| t.entries.get(id)) {
        return Some((entry.text.clone(), entry.args));
    }
    drop(cache);

    DEFAULT_ENTRIES
        .iter()
        .find(|(k, _, _)| *k == id)
        .map(|(_, text, args)| ((*text).to_string(), *args))
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
            let template = get_message(id, lang);
            substitute(&template, args)
        }
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

pub fn create_job_embed(job: &Job, payload: &MessagePayload) -> CreateEmbed {
    let lang = &job.lang;
    let mut status_message = format_payload(payload, lang);
    if let Some(eta) = active_encode_eta_text(payload) {
        if !status_message.to_ascii_lowercase().contains("eta") {
            status_message.push_str(&format!("\nETA: {}", eta));
        }
    }
    let preset_text = job
        .keep
        .as_ref()
        .and_then(|keep| {
            match (&keep.parent_keyword, &keep.output_keyword) {
                (Some(parent), Some(keyword)) => Some(format!("`{}` -> `{}`", parent, keyword)),
                _ => None,
            }
        })
        .unwrap_or_else(|| match &job.preset {
            Preset::PseudoLossless(Some(_)) => get_message(PRESET_PSEUDOLOSSLESS_INTRO, lang),
            Preset::PseudoLossless(None) => get_message(PRESET_PSEUDOLOSSLESS_NOINTRO, lang),
            Preset::Gpu(Some(_)) => get_message(PRESET_GPU_INTRO, lang),
            Preset::Gpu(None) => get_message(PRESET_GPU_NOINTRO, lang),
            Preset::Standard(Some(_)) => get_message(PRESET_STANDARD_INTRO, lang),
            Preset::Standard(None) => get_message(PRESET_STANDARD_NOINTRO, lang),
            Preset::Dummy(a) => format!("{} | {:?}", get_message(PRESET_DUMMY, lang), a),
        });

    let color = match job.ready {
        Stage::Queued => Colour::LIGHT_GREY,
        Stage::Probing => Colour::BLUE,
        Stage::Probed => Colour::DARK_BLUE,
        Stage::Downloading => Colour::BLUE,
        Stage::Downloaded => Colour::DARK_BLUE,
        Stage::Encoding => Colour::ORANGE,
        Stage::Encoded => Colour::DARK_ORANGE,
        Stage::Uploading => Colour::PURPLE,
        Stage::Uploaded => Colour::DARK_GREEN,
        Stage::Failed => Colour::RED,
        Stage::Declined => Colour::DARK_TEAL,
        Stage::Cancelled => Colour::DARK_GREY,
    };

    let mut embed = CreateEmbed::new()
        .title(substitute(
            &get_message(EMBED_TITLE, lang),
            &[PKGVER.to_string()],
        ))
        .colour(color)
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
            get_message(FIELD_STATUS, lang),
            get_stage_text(job.ready, lang),
            true,
        )
        .field(get_message(FIELD_PRESET, lang), preset_text, false)
        .field(
            get_message(FIELD_TORRENT, lang),
            job.display_link
                .clone()
                .unwrap_or_else(|| format!("{}", job.torrent.display())),
            false,
        );
    if !job.encode_warnings.is_empty() {
        embed = embed.field("Warnings", warnings_field(&job.encode_warnings), false);
    }
    embed
        .field(get_message(FIELD_PROGRESS, lang), status_message, false)
        .timestamp(serenity::model::Timestamp::now())
}

fn active_encode_eta_text(payload: &MessagePayload) -> Option<String> {
    let MessagePayload::Progress(id, args) = payload else {
        return None;
    };
    let (frame, total, fps) = if *id == ENCODE_PROG {
        (args.get(1)?, args.get(2)?, args.get(3)?)
    } else if *id == ENCODE_CONCAT_PROG {
        (args.get(0)?, args.get(1)?, args.get(2)?)
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

fn warnings_field(warnings: &[String]) -> String {
    let mut out = String::new();
    let mut hidden = 0usize;
    for warning in warnings {
        let next = if out.is_empty() {
            warning.clone()
        } else {
            format!("\n{}", warning)
        };
        if out.len() + next.len() > 1000 {
            hidden += 1;
        } else {
            out.push_str(&next);
        }
    }
    if hidden > 0 {
        out.push_str(&format!("\n...and {} more", hidden));
    }
    if out.is_empty() {
        "None".to_string()
    } else {
        out
    }
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
