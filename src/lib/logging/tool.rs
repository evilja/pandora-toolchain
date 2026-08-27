use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

// A write-through run log for the CLI tools.
//
// `LoggingHandle` buffers 5000 bytes before touching the disk, which is fine for an ffmpeg
// transcript and useless for the case that actually needs reading: a tool that never returns. When
// an encode stalled, the last thing anyone could see was that the encoder had been handed the job —
// what the tool did next left no trace at all. Every line here is written and flushed as it
// happens, and each carries the elapsed time since the tool started, so a log that stops tells you
// both where it stopped and how long it had been running.
pub struct ToolLog {
    file: Option<std::fs::File>,
    path: Option<PathBuf>,
    started: Instant,
}

impl ToolLog {
    pub fn open(path: Option<&str>) -> Self {
        let kept = path.map(PathBuf::from);
        let file = path.and_then(|path| {
            let path = Path::new(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            // Truncated for this run, then reopened for appending: a watchdog handle writes at the
            // end of the file, and a primary handle holding its own offset would write over those
            // lines the next time it logged.
            match std::fs::File::create(path)
                .and_then(|_| std::fs::OpenOptions::new().append(true).open(path))
            {
                Ok(file) => Some(file),
                Err(e) => {
                    eprintln!("[toollog] could not open {}: {}", path.display(), e);
                    None
                }
            }
        });
        Self {
            file,
            path: kept,
            started: Instant::now(),
        }
    }

    // A second handle on the same transcript, for a watchdog thread that has to keep reporting
    // while the main thread sits inside one long call. It appends rather than truncating, shares
    // the start instant so both stamp the same timeline, and relies on every line being flushed as
    // it is written so the two interleave instead of tearing.
    pub fn watchdog(&self) -> Self {
        let file = self.path.as_ref().and_then(|path| {
            std::fs::OpenOptions::new().append(true).open(path).ok()
        });
        Self {
            file,
            path: self.path.clone(),
            started: self.started,
        }
    }

    // Tools already receive a `--logfile` for the *subprocess* transcript (ffmpeg's stderr, curl's
    // progress). The run log sits beside it as `<name>.run.log` rather than taking a second CLI
    // parameter, so every existing call site gets one without touching its `CliParam` spec.
    pub fn beside(logfile: Option<&str>) -> Self {
        Self::open(logfile.map(run_log_path).as_deref())
    }

    pub fn line(&mut self, message: &str) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let _ = writeln!(
            file,
            "[{:>9.3}s] {}",
            self.started.elapsed().as_secs_f64(),
            message
        );
        let _ = file.flush();
    }

    // Convenience for the "about to do something that might never come back" pattern: log the
    // attempt, run it, log the outcome with how long it took.
    pub fn step<T>(&mut self, what: &str, run: impl FnOnce() -> T) -> T {
        self.line(&format!("-> {}", what));
        let at = Instant::now();
        let out = run();
        self.line(&format!("<- {} ({:.3}s)", what, at.elapsed().as_secs_f64()));
        out
    }

    pub fn is_enabled(&self) -> bool {
        self.file.is_some()
    }
}

fn run_log_path(logfile: &str) -> String {
    let path = PathBuf::from(logfile);
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.strip_suffix(".log").unwrap_or(name).to_string())
        .unwrap_or_else(|| "tool".to_string());
    path.with_file_name(format!("{}.run.log", stem))
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_log_sits_beside_the_tool_log() {
        assert_eq!(
            run_log_path("DB/work/12/log/PNmpeg_Encode12.log"),
            "DB/work/12/log/PNmpeg_Encode12.run.log"
        );
        // A name without the extension still gets one run log, not two suffixes.
        assert_eq!(run_log_path("DB/work/12/log/pncurl12"), "DB/work/12/log/pncurl12.run.log");
    }

    #[test]
    fn a_disabled_log_is_inert() {
        let mut log = ToolLog::open(None);
        assert!(!log.is_enabled());
        log.line("this goes nowhere");
        assert_eq!(log.step("work", || 21 * 2), 42);
    }

    #[test]
    fn lines_are_written_through_immediately() {
        let path = std::env::temp_dir().join(format!("pandora-toollog-{}.log", std::process::id()));
        std::fs::remove_file(&path).ok();
        let mut log = ToolLog::open(path.to_str());
        log.line("first");
        // Written before the handle is dropped — the point of the whole type.
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("first"), "{}", written);
        log.step("second", || {});
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("-> second") && written.contains("<- second"), "{}", written);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn a_watchdog_handle_and_the_primary_one_both_keep_their_lines() {
        let root = std::env::temp_dir().join(format!("pandora-toollog-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("run.log");
        let name = path.display().to_string();

        let mut log = ToolLog::open(Some(&name));
        let mut watchdog = log.watchdog();
        log.line("primary one");
        watchdog.line("watchdog one");
        // The primary handle logging after the watchdog is the case that used to overwrite: it
        // holds its own offset, which is short of where the watchdog left the file.
        log.line("primary two");

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("primary one"), "{written}");
        assert!(written.contains("watchdog one"), "{written}");
        assert!(written.contains("primary two"), "{written}");

        // Reopening the same path truncates, so a second run does not read as a continuation.
        let mut second = ToolLog::open(Some(&name));
        second.line("second run");
        let rerun = std::fs::read_to_string(&path).unwrap();
        assert!(!rerun.contains("primary one"), "{rerun}");
        assert!(rerun.contains("second run"), "{rerun}");
        std::fs::remove_dir_all(&root).ok();
    }
}
