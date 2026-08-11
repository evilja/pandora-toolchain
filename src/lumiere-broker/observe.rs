use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::LUMIERE_LOG_VERBOSE;
use reqwest::Url;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static VERBOSE: OnceLock<bool> = OnceLock::new();

// Every upload line is prefixed `[lumiere]` with a UTC time-of-day stamp and a
// scope, so a failing or stuck host can be followed in `docker compose logs pndc`
// without correlating against anything else.
pub(crate) fn info(scope: &str, message: impl AsRef<str>) {
    println!("[lumiere] {} {} | {}", stamp(), scope, message.as_ref());
}

pub(crate) fn warn(scope: &str, message: impl AsRef<str>) {
    eprintln!("[lumiere] {} {} | {}", stamp(), scope, message.as_ref());
}

// Per-chunk and per-poll detail: too noisy for a busy release, useful when one
// host is being chased. Enabled with `lumiere_log_verbose|pntools|true`.
pub(crate) fn trace(scope: &str, message: impl AsRef<str>) {
    if verbose() {
        println!("[lumiere] {} {} | {}", stamp(), scope, message.as_ref());
    }
}

pub(crate) fn verbose() -> bool {
    *VERBOSE.get_or_init(|| {
        get_pandora_env()
            .get(LUMIERE_LOG_VERBOSE)
            .map(|value| value.trim().to_ascii_lowercase())
            .map(|value| matches!(value.as_str(), "1" | "true" | "on" | "yes" | "verbose"))
            .unwrap_or(false)
    })
}

// Logs must never carry a capability token, a Drive `upload_id`, or a bearer
// token, because operator logs are shipped and kept far more casually than
// secrets are. Host and path shape are what actually diagnose a broken host.
pub(crate) fn redact_url(raw: &str) -> String {
    let Ok(url) = Url::parse(raw) else {
        return "<unparsable-url>".to_string();
    };
    let mut out = format!("{}://{}", url.scheme(), url.host_str().unwrap_or("?"));
    if let Some(port) = url.port() {
        out.push_str(&format!(":{port}"));
    }
    if let Some(segments) = url.path_segments() {
        for segment in segments {
            out.push('/');
            out.push_str(&mask(segment));
        }
    }
    let keys = url
        .query_pairs()
        .map(|(key, _)| format!("{key}=…"))
        .collect::<Vec<_>>();
    if !keys.is_empty() {
        out.push('?');
        out.push_str(&keys.join("&"));
    }
    out
}

// A short, non-reversible handle for a capability token so one transfer can be
// followed across registration, provider fetches, and completion.
pub(crate) fn token_tag(token: &str) -> String {
    let head = token.chars().take(8).collect::<String>();
    format!("{head}…")
}

// Masks path segments that look like credentials while leaving readable parts —
// route names and the filename itself — alone, because those are what make a log
// line worth reading.
pub(crate) fn mask(value: &str) -> String {
    let opaque = value.len() > 20
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if !opaque {
        return value.to_string();
    }
    format!("{}…", value.chars().take(6).collect::<String>())
}

pub(crate) fn bytes_label(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2}GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

pub(crate) fn duration_label(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds >= 3600 {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}.{}s", seconds, elapsed.subsec_millis() / 100)
    }
}

pub(crate) fn rate_label(bytes: u64, elapsed: Duration) -> String {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 || bytes == 0 {
        return "0B/s".to_string();
    }
    format!("{}/s", bytes_label((bytes as f64 / seconds) as u64))
}

// reqwest's Display hides the interesting part behind a source chain, and a hung
// host looks very different from a refused one in these logs.
pub(crate) fn transport_reason(error: &reqwest::Error) -> String {
    let mut reason = if error.is_timeout() {
        "timeout".to_string()
    } else if error.is_connect() {
        "connect failed".to_string()
    } else if error.is_body() {
        "body error".to_string()
    } else {
        "request error".to_string()
    };
    reason.push_str(&format!(": {error}"));
    if let Some(source) = std::error::Error::source(error) {
        reason.push_str(&format!(" ({source})"));
    }
    reason
}

// Provider and Google failure bodies carry the actual cause; keep a short excerpt
// so the log says why instead of only that something was rejected.
pub(crate) async fn response_body_excerpt(response: reqwest::Response) -> String {
    let body = response.text().await.unwrap_or_default();
    let body = body.trim();
    if body.is_empty() {
        return "<empty body>".to_string();
    }
    let mut excerpt = body.chars().take(300).collect::<String>();
    if body.chars().count() > 300 {
        excerpt.push('…');
    }
    excerpt.replace('\n', " ")
}

fn stamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}Z",
        (seconds / 3600) % 24,
        (seconds / 60) % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_and_session_urls_lose_their_secrets() {
        let capability = redact_url(&format!(
            "https://files.example/lumiere/v1/files/{}/episode 01.mp4",
            "a".repeat(64)
        ));
        assert!(!capability.contains(&"a".repeat(64)));
        assert_eq!(
            capability,
            "https://files.example/lumiere/v1/files/aaaaaa…/episode%2001.mp4"
        );
        let session = redact_url(
            "https://www.googleapis.com/upload/drive/v3/files?uploadType=resumable&upload_id=SECRET",
        );
        assert!(!session.contains("SECRET"));
        assert!(session.ends_with("?uploadType=…&upload_id=…"));
        assert!(session.starts_with("https://www.googleapis.com/upload/drive/v3/files"));
    }

    #[test]
    fn masking_keeps_readable_segments() {
        assert_eq!(mask("files"), "files");
        assert_eq!(mask("[AkiraSubs] Frieren - 01.mp4"), "[AkiraSubs] Frieren - 01.mp4");
        assert_eq!(mask(&"f".repeat(64)), "ffffff…");
    }

    #[test]
    fn labels_stay_readable_at_release_sizes() {
        assert_eq!(bytes_label(0), "0B");
        assert_eq!(bytes_label(3 * 1024 * 1024), "3.0MB");
        assert_eq!(bytes_label(2 * 1024 * 1024 * 1024), "2.00GB");
        assert_eq!(duration_label(Duration::from_secs(95)), "1m35s");
        assert_eq!(duration_label(Duration::from_millis(2500)), "2.5s");
        assert_eq!(
            rate_label(10 * 1024 * 1024, Duration::from_secs(5)),
            "2.0MB/s"
        );
    }
}
