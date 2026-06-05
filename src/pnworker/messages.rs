use crate::pnworker::core::{Job, Preset, Stage};
use serenity::all::{Colour, CreateEmbed};

const PKGVER: &'static str = env!("CARGO_PKG_VERSION");

pub const QUEUE_TOO_LONG: usize = 0;
pub const QUEUED: usize = 1;
pub const JOB_CANCELLED: usize = 2;
pub const PROBE_TIMEOUT: usize = 3;
pub const GITSYNC_PROGRESS: usize = 4;
pub const GITSYNC_SUCCESS: usize = 5;
pub const GITSYNC_FAIL: usize = 6;

pub const CTORRENT_DONE: usize = 7;
pub const CTORRENT_FAIL: usize = 8;
pub const TORRENT_PROG: usize = 9;
pub const TORRENT_PROG_SELECT: usize = 10;
pub const TORRENT_DONE: usize = 11;
pub const TORRENT_FAIL: usize = 12;

pub const ENCODE_PROG: usize = 13;
pub const ENCODE_CONCAT_PROG: usize = 14;
pub const ENCODE_DONE: usize = 15;
pub const ENCODE_FAIL: usize = 16;

pub const UPLOAD_PROG: usize = 17;
pub const UPLOAD_DONE: usize = 18;
pub const UPLOAD_FAIL: usize = 19;

pub const PROBE_DONE: usize = 20;
pub const PROBE_FAIL: usize = 21;
pub const PROBE_ROW: usize = 22;

pub const EMBED_TITLE: usize = 23;
pub const FIELD_JOBID: usize = 24;
pub const FIELD_AUTHOR: usize = 25;
pub const FIELD_STATUS: usize = 26;
pub const FIELD_PRESET: usize = 27;
pub const FIELD_TORRENT: usize = 28;
pub const FIELD_PROGRESS: usize = 29;

pub const STAGE_QUEUED: usize = 30;
pub const STAGE_PROBING: usize = 31;
pub const STAGE_PROBED: usize = 32;
pub const STAGE_DOWNLOADING: usize = 33;
pub const STAGE_DOWNLOADED: usize = 34;
pub const STAGE_ENCODING: usize = 35;
pub const STAGE_ENCODED: usize = 36;
pub const STAGE_UPLOADING: usize = 37;
pub const STAGE_UPLOADED: usize = 38;
pub const STAGE_FAILED: usize = 39;
pub const STAGE_DECLINED: usize = 40;
pub const STAGE_CANCELLED: usize = 41;

pub const PRESET_PSEUDOLOSSLESS_INTRO: usize = 42;
pub const PRESET_PSEUDOLOSSLESS_NOINTRO: usize = 43;
pub const PRESET_GPU_INTRO: usize = 44;
pub const PRESET_GPU_NOINTRO: usize = 45;
pub const PRESET_STANDARD_INTRO: usize = 46;
pub const PRESET_STANDARD_NOINTRO: usize = 47;
pub const PRESET_DUMMY: usize = 48;

pub const MESSAGE_COUNT: usize = 49;

pub const DEFAULT_LANGS: &[&str] = &["en", "tr", "jp"];

static DEFAULT_MESSAGES: &[&str] = &[
    "\n\nŞu anda Pandora Toolchain'de biraz sıra var. \nLütfen daha sonra tekrar deneyin.",
    "\n\nİsteğiniz alındı.",
    "\nİşlem iptal edildi.",
    "Probe timed out. use /pancode within 3 minutes next time.",
    "Tüm işlemler kapatılıyor.",
    "Kaynak kodlar git ile güncellendi.\nBot yeniden başlatılıyor.",
    "Git güncellemesi başarısız oldu.\nBot yine de yeniden başlatılıyor.",
    "\n\nTorrent kısa süre içinde indirilmeye başlanacak.",
    "\n\nTorrent metadatası indirilemedi.",
    "\n\nTorrent ilerlemesi: {}% {}MB/{}MB",
    "\n\nTorrent ilerlemesi: {}% {}MB",
    "\n\nEncode kısa süre içinde başlayacak.",
    "\n\nTorrent indirilemedi.",
    "\n\nDosya encode ediliyor.\nAşama: 1/{}\nİşlenen kare: {}/{}\nSaniye başına işlenen kare: {}\nSaniye başına ortalama veri: {}kbit/s",
    "\n\nDosyaya intro ekleniyor.\nAşama: 2/2\nİşlenen kare: {}/{}\nSaniye başına işlenen kare: {}\nSaniye başına ortalama veri: {}kbit/s",
    "\n\nÇıktı sunuculara yükleniyor.",
    "\n\nDosya encode edilemedi.",
    "\n\nYükleme ilerlemesi:\n{}\n{}\n{}\n{}\n{}",
    "\n\nDosya yüklendi.\n{}\n{}\n{}\n{}\n{}",
    "\n\nDosya yüklenemedi. \nBir yetkiliden botu yeniden başlatmasını isteyebilirsiniz.",
    "\n\nBatch torrent incelendi.",
    "\n\nBatch torrent incelenemedi.",
    "\n\nDosya numaraları:\n{}",
    "Encode İşlemi ({})",
    "İşlem Numarası",
    "İşlem Sahibi",
    "Durum",
    "Encode Preset",
    "Torrent Linki",
    "İlerleme",
    "Sırada",
    "İnceleniyor",
    "İncelendi",
    "İndiriliyor",
    "İndirildi",
    "Encode Ediliyor",
    "Encode Edildi",
    "Yükleniyor",
    "Tamamlandı",
    "Başarısız",
    "Reddedildi",
    "İptal Edildi",
    "Kayıpsız - İşlemci | İntrolu",
    "Kayıpsız - İşlemci | İntrosuz",
    "Standart - Ekran kartı | İntrolu",
    "Standart - Ekran kartı | İntrosuz",
    "Standart - İşlemci | İntrolu",
    "Standart - İşlemci | İntrosuz",
    "DEVELOPER",
];

pub fn init_language_files() {
    for lang in DEFAULT_LANGS {
        let path = format!("DB/config/{}.pandora", lang);
        if std::path::Path::new(&path).exists() {
            continue;
        }
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let body: String = DEFAULT_MESSAGES.iter()
            .map(|s| format!("{}\n", s))
            .collect();
        let _ = std::fs::write(&path, body);
    }
}

pub fn get_message(id: usize, lang: &str) -> String {
    let path = format!("DB/config/{}.pandora", lang);
    if let Ok(content) = std::fs::read_to_string(&path) {
        let lines: Vec<&str> = content.lines().collect();
        if let Some(line) = lines.get(id) {
            return (*line).to_string();
        }
    }
    DEFAULT_MESSAGES.get(id).map(|s| (*s).to_string()).unwrap_or_default()
}

#[derive(Clone, Debug)]
pub enum MessagePayload {
    Static(usize),
    Progress(usize, Vec<String>),
}

pub fn format_payload(payload: &MessagePayload, lang: &str) -> String {
    match payload {
        MessagePayload::Static(id) => get_message(*id, lang),
        MessagePayload::Progress(id, args) => {
            let template = get_message(*id, lang);
            substitute(&template, args)
        }
    }
}

fn substitute(template: &str, args: &[String]) -> String {
    let mut result = template.to_string();
    for arg in args {
        if let Some(pos) = result.find("{}") {
            result.replace_range(pos..pos+2, arg);
        }
    }
    result
}

pub fn create_job_embed(job: &Job, payload: &MessagePayload) -> CreateEmbed {
    let lang = &job.lang;
    let status_message = format_payload(payload, lang);
    let preset_text = match &job.preset {
        Preset::PseudoLossless(Some(_)) => get_message(PRESET_PSEUDOLOSSLESS_INTRO, lang),
        Preset::PseudoLossless(None) => get_message(PRESET_PSEUDOLOSSLESS_NOINTRO, lang),
        Preset::Gpu(Some(_)) => get_message(PRESET_GPU_INTRO, lang),
        Preset::Gpu(None) => get_message(PRESET_GPU_NOINTRO, lang),
        Preset::Standard(Some(_)) => get_message(PRESET_STANDARD_INTRO, lang),
        Preset::Standard(None) => get_message(PRESET_STANDARD_NOINTRO, lang),
        Preset::Dummy(a) => format!("{} | {:?}", get_message(PRESET_DUMMY, lang), a),
    };

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

    CreateEmbed::new()
        .title(get_message(EMBED_TITLE, lang).replace("{}", PKGVER))
        .colour(color)
        .field(get_message(FIELD_JOBID, lang), format!("`{}`", job.job_id), true)
        .field(get_message(FIELD_AUTHOR, lang), format!("<@{}>", job.author), true)
        .field(get_message(FIELD_STATUS, lang), get_stage_text(job.ready, lang), true)
        .field(get_message(FIELD_PRESET, lang), preset_text, false)
        .field(get_message(FIELD_TORRENT, lang), format!("Torrent: {}", job.torrent.display()), false)
        .field(get_message(FIELD_PROGRESS, lang), status_message, false)
        .timestamp(serenity::model::Timestamp::now())
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
