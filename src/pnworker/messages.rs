use crate::pnworker::core::{Job, Preset};
use serenity::all::{CreateEmbed, Colour};
use crate::pnworker::core::{Stage};

/// Create a Discord embed for job status
pub fn create_job_embed(job: &Job, status_message: &str) -> CreateEmbed {
    let preset_text = match job.preset {
        Preset::PseudoLossless(a) => {
            format!("Kayıpsız - İşlemci | {}",
                if a.is_some() { "İntrolu" } else { "İntrosuz" })
        },
        Preset::Gpu(a) => {
            format!("Standart - Ekran kartı | {}",
                if a.is_some() { "İntrolu" } else { "İntrosuz" })
        },
        Preset::Standard(a) => {
            format!("Standart - İşlemci | {}",
                if a.is_some() { "İntrolu" } else { "İntrosuz" })
        },
    };

    // Choose color based on stage
    let color = match job.ready {
        Stage::Queued => Colour::LIGHT_GREY,
        Stage::Downloading => Colour::BLUE,
        Stage::Downloaded => Colour::DARK_BLUE,
        Stage::Encoding => Colour::ORANGE,
        Stage::Encoded => Colour::DARK_ORANGE,
        Stage::Uploading => Colour::PURPLE,
        Stage::Uploaded => Colour::DARK_GREEN,
        Stage::Failed => Colour::RED,
        Stage::Declined => Colour::RED,
        Stage::Cancelled => Colour::RED,
    };

    CreateEmbed::new()
        .title("Encode İşlemi")
        .colour(color)
        .field("İşlem Numarası", format!("`{}`", job.job_id), true)
        .field("İşlem Sahibi", format!("<@{}>", job.author), true)
        .field("Durum", get_stage_emoji(job.ready), true)
        .field("Encode Preset", preset_text, false)
        .field("Torrent Linki", format!("Torrent: {}", job.torrent.display()), false)
        .field("İlerleme", status_message, false)
        .timestamp(serenity::model::Timestamp::now())
}

/// Get emoji for stage
fn get_stage_emoji(stage: Stage) -> &'static str {
    match stage {
        Stage::Queued => "Sırada",
        Stage::Downloading => "İndiriliyor",
        Stage::Downloaded => "İndirildi",
        Stage::Encoding => "Encode Ediliyor",
        Stage::Encoded => "Encode Edildi",
        Stage::Uploading => "Yükleniyor",
        Stage::Uploaded => "Tamamlandı",
        Stage::Failed => "Başarısız",
        Stage::Declined => "Reddedildi",
        Stage::Cancelled => "İptal Edildi",
    }
}

pub const QUEUE_TOO_LONG: &str = "\n\nŞu anda Pandora Toolchain'de biraz sıra var. \nLütfen daha sonra tekrar deneyin.";
pub const QUEUED: &str = "\n\nİsteğiniz alındı.";
pub const HEADER_JOBID: &str = "İşlem numarası: ";
pub const HEADER_AUTID: &str = "\nİşlem sahibi: ";
pub const HEADER_TORRN: &str = "\nTorrent linki: ";
pub const HEADER_PREST: &str = "\nEncode preset: ";

pub const JOB_CANCELLED: &str = "\nİşlem iptal edildi.";

pub const CTORRENT_DONE: &str = "\n\nTorrent metadatası indirildi.\nTorrentin kendisini indiriliyor.\nİlerleme: Torrent başlatılıyor.";
pub const CTORRENT_FAIL: &str = "\n\nTorrent metadatası indirilemedi.\nÇıkılıyor.";
pub const TORRENT_PROG: &str = "\n\nTorrent metadatası indirildi.\nTorrentin kendisini indiriliyor.\nİlerleme:";
pub const TORRENT_DONE: &str = "\n\nTorrentin kendisi indirildi.\nUygun olunduğunda encode işlemine geçilecek.";
pub const TORRENT_FAIL: &str = "\n\nTorrentin kendisi indirilemedi. \nÇıkılıyor.";
pub const ENCODE_PROG: &str = "\n\nDosya encode ediliyor.\n";
pub const ENCODE_CONCAT_PROG: &str = "\n\nDosyaya intro ekleniyor.\n";
pub const ENCODE_DONE: &str = "\n\nDosya encode edildi.\nGDrive'a yükleniyor.";
pub const ENCODE_FAIL: &str = "\n\nDosya encode edilemedi. \nÇıkılıyor.";
pub const UPLOAD_PROG: &str = "\n\nDosya GDrive'a yükleniyor.\nİlerleme:";
pub const UPLOAD_DONE: &str = "\n\nDosya GDrive'a yüklendi. \nLink:";
pub const UPLOAD_FAIL: &str = "\n\nDosya GDrive'a yüklenemedi. \nÇıktı dosyası makineden 24:00'da silinecektir.\nİsterseniz dosyayı şu konumdan kendiniz alabilirsiniz:";
