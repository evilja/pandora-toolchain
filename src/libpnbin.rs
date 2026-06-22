use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::libpnenv::core::{get_pandora_env, upsert_env};
use crate::libpnenv::standard::{ENV_PATH, PNASS, PNCURL, PNMPEG, PNP2P};

pub fn runtime_binary_path(name: &str) -> PathBuf {
    PathBuf::from("DB").join("bin").join(platform_binary_name(name))
}

pub fn resolve_runtime_binary(name: &str) -> PathBuf {
    let local = runtime_binary_path(name);
    if local.is_file() {
        local
    } else {
        PathBuf::from(platform_binary_name(name))
    }
}

pub async fn ensure_startup_binaries() {
    if let Err(e) = tokio::fs::create_dir_all("DB/bin").await {
        eprintln!("Warning: failed to create DB/bin: {}", e);
        return;
    }

    ensure_tool_env_paths();

    if command_available("ffmpeg") && command_available("ffprobe") {
        println!("Runtime binary check: ffmpeg and ffprobe found in PATH");
        return;
    }

    if local_binary_available("ffmpeg") && local_binary_available("ffprobe") {
        println!("Runtime binary check: ffmpeg and ffprobe found in DB/bin");
        return;
    }

    println!("Runtime binary check: ffmpeg/ffprobe missing, downloading portable build");
    match download_portable_ffmpeg().await {
        Ok(()) => println!("Runtime binary check: portable ffmpeg installed in DB/bin"),
        Err(e) => eprintln!("Warning: failed to install portable ffmpeg: {}", e),
    }
}

fn platform_binary_name(name: &str) -> String {
    if cfg!(windows) && !name.ends_with(".exe") {
        format!("{}.exe", name)
    } else {
        name.to_string()
    }
}

fn command_available(name: &str) -> bool {
    Command::new(platform_binary_name(name))
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn local_binary_available(name: &str) -> bool {
    let path = runtime_binary_path(name);
    if !path.is_file() {
        return false;
    }
    Command::new(path)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn ensure_tool_env_paths() {
    let env = get_pandora_env();
    for (key, bin) in [(PNMPEG, "pnmpeg"), (PNP2P, "pnp2p"), (PNCURL, "pncurl"), (PNASS, "pnass")] {
        let current = env.get(key).map(|v| v.trim()).unwrap_or("");
        if !current.is_empty() && tool_invocation_available(current) {
            continue;
        }
        if let Some(path) = find_sibling_tool(bin) {
            if let Err(e) = upsert_env(ENV_PATH, key, &path.display().to_string()) {
                eprintln!("Warning: failed to update {} path: {}", key, e);
            } else {
                println!("Runtime binary check: set {} to {}", key, path.display());
            }
        } else if current.is_empty() {
            eprintln!("Warning: {} is not configured and {} was not found next to pndc", key, bin);
        } else {
            eprintln!("Warning: configured {} path does not exist: {}", key, current);
        }
    }
}

fn tool_invocation_available(path: &str) -> bool {
    if Path::new(path).is_file() {
        return true;
    }
    Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn find_sibling_tool(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidate = dir.join(platform_binary_name(name));
    if candidate.is_file() {
        return Some(candidate);
    }
    let local = runtime_binary_path(name);
    if local.is_file() {
        return Some(local);
    }
    None
}

async fn download_portable_ffmpeg() -> Result<(), Box<dyn std::error::Error>> {
    let url = portable_ffmpeg_url().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("portable ffmpeg download is not configured for {}-{}", std::env::consts::OS, std::env::consts::ARCH),
        )
    })?;

    let cache_dir = PathBuf::from("DB/bin/cache");
    let extract_dir = cache_dir.join("ffmpeg_extract");
    let archive = cache_dir.join("ffmpeg.tar.xz");
    let _ = tokio::fs::remove_dir_all(&extract_dir).await;
    tokio::fs::create_dir_all(&extract_dir).await?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(900))
        .build()?;
    let bytes = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    tokio::fs::write(&archive, &bytes).await?;

    let status = tokio::process::Command::new("tar")
        .arg("-xJf")
        .arg(&archive)
        .arg("-C")
        .arg(&extract_dir)
        .status()
        .await?;
    if !status.success() {
        return Err(std::io::Error::new(std::io::ErrorKind::Other, "tar failed to extract ffmpeg archive").into());
    }

    let ffmpeg = find_file_named(&extract_dir, "ffmpeg")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "ffmpeg missing from archive"))?;
    let ffprobe = find_file_named(&extract_dir, "ffprobe")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "ffprobe missing from archive"))?;

    tokio::fs::copy(ffmpeg, runtime_binary_path("ffmpeg")).await?;
    tokio::fs::copy(ffprobe, runtime_binary_path("ffprobe")).await?;
    make_executable(&runtime_binary_path("ffmpeg"))?;
    make_executable(&runtime_binary_path("ffprobe"))?;
    let _ = tokio::fs::remove_dir_all(&extract_dir).await;
    Ok(())
}

fn portable_ffmpeg_url() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz")
    } else if cfg!(all(target_os = "linux", target_arch = "arm")) {
        Some("https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-armhf-static.tar.xz")
    } else {
        None
    }
}

fn find_file_named(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file_named(&path, name) {
                return Some(found);
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}
