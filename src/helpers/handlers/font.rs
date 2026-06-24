use super::*;

pub async fn handle_font(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/font").await {
        Some(id) => id,
        None => return,
    };

    let attachment = option_attachment(command, "file");
    let has_attachment = attachment.is_some();
    let link = option_trimmed(command, "link");

    if attachment.is_none() && link.is_none() {
        command_error(ctx, command, "Error: provide either `file` or `link`.").await;
        return;
    }

    if command.create_response(ctx, CreateInteractionResponse::Defer(
        CreateInteractionResponseMessage::new().ephemeral(true)
    )).await.is_err() {
        return;
    }

    let zip_bytes = if let Some(a) = attachment {
        let name = a.filename.to_lowercase();
        if !name.ends_with(".zip") {
            font_response(ctx, command, "Error: `file` must be a .zip archive.").await;
            return;
        }
        match a.download().await {
            Ok(b) => b,
            Err(e) => {
                font_response(ctx, command, format!("Failed to download attachment: {}", e)).await;
                return;
            }
        }
    } else {
        let url = link.unwrap();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            font_response(ctx, command, "Error: `link` must be an http(s) URL.").await;
            return;
        }
        let resp = match reqwest::get(&url).await {
            Ok(r) => r,
            Err(e) => {
                font_response(ctx, command, format!("Failed to fetch zip: {}", e)).await;
                return;
            }
        };
        if !resp.status().is_success() {
            font_response(ctx, command, format!("Failed to fetch zip: HTTP {}", resp.status())).await;
            return;
        }
        match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                font_response(ctx, command, format!("Failed to read zip body: {}", e)).await;
                return;
            }
        }
    };

    let dir = std::path::PathBuf::from("DB")
        .join("fontconfig")
        .join(server_id.to_string());
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        font_response(ctx, command, format!("Failed to create font dir: {}", e)).await;
        return;
    }

    let count = match extract_zip_to_dir(&zip_bytes, &dir).await {
        Ok(c) => c,
        Err(e) => {
            font_response(ctx, command, format!("Failed to extract zip: {}", e)).await;
            return;
        }
    };

    let source = if has_attachment { "attachment" } else { "link" };
    let mut message = format!("Extracted {} file(s) from {} into `{}`.", count, source, dir.display());
    match install_fonts_to_linux_folder(&dir, server_id).await {
        Ok(Some(installed)) => {
            let cache = if installed.cache_refreshed { "refreshed" } else { "not refreshed" };
            message.push_str(&format!(" Installed {} font file(s) to `{}` (font cache {}).", installed.count, installed.dir.display(), cache));
        }
        Ok(None) => {}
        Err(e) => message.push_str(&format!(" Linux font install failed: {}", e)),
    }
    font_response(ctx, command, message).await;
}

struct LinuxFontInstall {
    dir: PathBuf,
    count: usize,
    cache_refreshed: bool,
}

#[cfg(target_os = "linux")]
async fn install_fonts_to_linux_folder(src: &Path, server_id: u64) -> Result<Option<LinuxFontInstall>, String> {
    let dir = linux_font_dir(server_id).await?;
    let count = copy_font_files(src, &dir).await?;
    let cache_refreshed = if count > 0 {
        tokio::process::Command::new("fc-cache")
            .arg("-f")
            .arg(&dir)
            .output()
            .await
            .map(|out| out.status.success())
            .unwrap_or(false)
    } else {
        false
    };
    Ok(Some(LinuxFontInstall { dir, count, cache_refreshed }))
}

#[cfg(not(target_os = "linux"))]
async fn install_fonts_to_linux_folder(_src: &Path, _server_id: u64) -> Result<Option<LinuxFontInstall>, String> {
    Ok(None)
}

#[cfg(target_os = "linux")]
async fn linux_font_dir(server_id: u64) -> Result<PathBuf, String> {
    let system = PathBuf::from("/usr/local/share/fonts").join("pandora").join(server_id.to_string());
    match tokio::fs::create_dir_all(&system).await {
        Ok(()) => return Ok(system),
        Err(system_err) => {
            let base = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share")))
                .ok_or_else(|| format!("{} and no user font directory is available", system_err))?;
            let user = base.join("fonts").join("pandora").join(server_id.to_string());
            tokio::fs::create_dir_all(&user).await
                .map_err(|e| format!("{}; fallback {}: {}", system_err, user.display(), e))?;
            Ok(user)
        }
    }
}

#[cfg(target_os = "linux")]
async fn copy_font_files(src: &Path, dest: &Path) -> Result<usize, String> {
    let mut pending = vec![src.to_path_buf()];
    let mut count = 0usize;
    while let Some(dir) = pending.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await.map_err(|e| e.to_string())?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
            let path = entry.path();
            let kind = entry.file_type().await.map_err(|e| e.to_string())?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && is_font_file(&path) {
                let rel = path.strip_prefix(src).map_err(|e| e.to_string())?;
                let out = dest.join(rel);
                if let Some(parent) = out.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
                }
                tokio::fs::copy(&path, &out).await.map_err(|e| e.to_string())?;
                count += 1;
            }
        }
    }
    Ok(count)
}

#[cfg(target_os = "linux")]
fn is_font_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("ttf" | "otf" | "ttc" | "otc" | "woff" | "woff2")
    )
}
