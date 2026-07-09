use std::path::Path;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::lib::bin::resolve_runtime_binary;

pub async fn ffmpeg_screenshot(
    input: &Path,
    subs: &Path,
    fontsdir: &Path,
    centiseconds: u64,
    out: &Path,
) -> Result<(), String> {
    let seek = format!("{:.2}", centiseconds as f64 / 100.0);
    let filter = format!(
        "subtitles=f='{}':fontsdir='{}'",
        escape_filter_path(subs),
        escape_filter_path(fontsdir)
    );
    let mut cmd = Command::new(resolve_runtime_binary("ffmpeg"));
    cmd.kill_on_drop(true)
        .arg("-y")
        .arg("-ss")
        .arg(seek)
        .arg("-copyts")
        .arg("-i")
        .arg(input)
        .arg("-vf")
        .arg(filter)
        .arg("-frames:v")
        .arg("1")
        .arg("-update")
        .arg("1")
        .arg(out);

    let output = match timeout(Duration::from_secs(120), cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(e.to_string()),
        Err(_) => return Err("ffmpeg screenshot timed out".to_string()),
    };
    if !output.status.success() {
        return Err(format!(
            "ffmpeg exited with {}; {}",
            output.status,
            stderr_tail(&output.stderr)
        ));
    }
    if !out.exists() {
        return Err("ffmpeg produced no frame".to_string());
    }
    Ok(())
}

pub fn escape_filter_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .flat_map(|ch| match ch {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            ':' => "\\:".chars().collect::<Vec<_>>(),
            '\'' => "\\'".chars().collect::<Vec<_>>(),
            ',' => "\\,".chars().collect::<Vec<_>>(),
            _ => vec![ch],
        })
        .collect()
}

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let tail = text
        .lines()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    if tail.trim().is_empty() {
        "no stderr".to_string()
    } else {
        tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn escape_filter_path_escapes_filter_separators() {
        let path = PathBuf::from("/tmp/a:b,c'd\\e.ass");
        assert_eq!(escape_filter_path(&path), "/tmp/a\\:b\\,c\\'d\\\\e.ass");
    }
}
