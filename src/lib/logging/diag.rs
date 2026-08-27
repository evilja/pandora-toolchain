use std::path::{Path, PathBuf};
use std::process::ExitStatus;

// Facts that only matter once something has already died: what the host had left, how large the
// process that vanished had grown, and whether an exit was a signal rather than a return code.
// A speculative encoder that is killed leaves no message of its own — without these lines the log
// says "the encode failed" where it could have said "the kernel killed it at 7.3 GiB".

fn meminfo_field(meminfo: &str, field: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        line.strip_prefix(field)?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })
}

// One log-line fragment describing what the host has left to give. Hosts without /proc get a
// marker rather than a fabricated number.
pub fn memory_line() -> String {
    let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") else {
        return "mem=unknown".to_string();
    };
    let Some(available) = meminfo_field(&meminfo, "MemAvailable:") else {
        return "mem=unknown".to_string();
    };
    format!(
        "mem_available={}MiB mem_total={}MiB swap_free={}MiB",
        available / 1024,
        meminfo_field(&meminfo, "MemTotal:").unwrap_or(0) / 1024,
        meminfo_field(&meminfo, "SwapFree:").unwrap_or(0) / 1024,
    )
}

// Resident size of another process, so a log can record how big a child had grown while it was
// still alive. Reads as None the moment that process is gone, which is why callers that care about
// the size at death have to sample it as they go.
pub fn process_rss_mib(pid: u32) -> Option<u64> {
    let status =
        std::fs::read_to_string(PathBuf::from("/proc").join(pid.to_string()).join("status")).ok()?;
    status.lines().find_map(|line| {
        Some(line.strip_prefix("VmRSS:")?.split_whitespace().next()?.parse::<u64>().ok()? / 1024)
    })
}

// `ExitStatus` renders a signal as "signal: 9 (SIGKILL)" and leaves the reader to know what that
// implies. An encoder killed by SIGKILL with no cancel file in sight is the OOM killer nearly every
// time, and that is the sentence worth having in the transcript.
pub fn exit_reason(status: &ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let hint = if signal == 9 {
                " (SIGKILL — what the OOM killer sends)"
            } else {
                ""
            };
            return format!("killed by signal {signal}{hint}");
        }
    }
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => status.to_string(),
    }
}

// The last few lines of a captured stderr file, collapsed onto one line. Tool logs are read with
// grep and every event is one line, so a multi-line ffmpeg complaint has to be flattened before it
// is quoted into an error.
pub fn tail_line(path: &Path, lines: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return "no stderr captured".to_string();
    };
    let tail: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(lines)
        .collect();
    if tail.is_empty() {
        return "stderr empty".to_string();
    }
    tail.into_iter().rev().collect::<Vec<&str>>().join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_fields_parse_out_of_a_sample() {
        let sample = "MemTotal:       16384000 kB\nMemFree:          102400 kB\nMemAvailable:    3145728 kB\n";
        assert_eq!(meminfo_field(sample, "MemAvailable:"), Some(3_145_728));
        assert_eq!(meminfo_field(sample, "MemTotal:"), Some(16_384_000));
        assert_eq!(meminfo_field(sample, "SwapFree:"), None);
    }

    #[test]
    fn tail_collapses_to_one_line_and_survives_a_missing_file() {
        let root = std::env::temp_dir().join(format!("pandora-diag-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("stderr.log");
        std::fs::write(&path, "first\n\nsecond\nthird\n").unwrap();
        assert_eq!(tail_line(&path, 2), "second | third");
        assert_eq!(tail_line(&root.join("absent.log"), 2), "no stderr captured");
        std::fs::write(&path, "\n\n").unwrap();
        assert_eq!(tail_line(&path, 2), "stderr empty");
        std::fs::remove_dir_all(&root).ok();
    }
}
