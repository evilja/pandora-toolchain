use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::lib::env::core::{get_pandora_env, upsert_env};
use crate::lib::env::standard::{ENV_PATH, PNASS, PNCURL, PNMPEG, PNP2P};

enum ArchiveKind {
    TarXz,
    Zip,
}

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
    let (url, kind, archive_name) = portable_ffmpeg_download().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("portable ffmpeg download is not configured for {}-{}", std::env::consts::OS, std::env::consts::ARCH),
        )
    })?;

    let cache_dir = PathBuf::from("DB/bin/cache");
    let extract_dir = cache_dir.join("ffmpeg_extract");
    let archive = cache_dir.join(archive_name);
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

    match kind {
        ArchiveKind::TarXz => {
            extract_tar_xz(&archive, &extract_dir).await?;
            copy_extracted_binaries(&extract_dir).await?;
        }
        ArchiveKind::Zip => {
            extract_zip_binaries(&archive).await?;
        }
    }

    let _ = tokio::fs::remove_dir_all(&extract_dir).await;
    Ok(())
}

fn portable_ffmpeg_download() -> Option<(&'static str, ArchiveKind, &'static str)> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(("https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz", ArchiveKind::TarXz, "ffmpeg.tar.xz"))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(("https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz", ArchiveKind::TarXz, "ffmpeg.tar.xz"))
    } else if cfg!(all(target_os = "linux", target_arch = "arm")) {
        Some(("https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-armhf-static.tar.xz", ArchiveKind::TarXz, "ffmpeg.tar.xz"))
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(("https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip", ArchiveKind::Zip, "ffmpeg.zip"))
    } else {
        None
    }
}

async fn extract_tar_xz(archive: &Path, extract_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = tokio::process::Command::new("tar")
        .arg("-xJf")
        .arg(archive)
        .arg("-C")
        .arg(extract_dir)
        .status()
        .await?;
    if !status.success() {
        return Err(std::io::Error::new(std::io::ErrorKind::Other, "tar failed to extract ffmpeg archive").into());
    }
    Ok(())
}

async fn copy_extracted_binaries(extract_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let ffmpeg_name = platform_binary_name("ffmpeg");
    let ffprobe_name = platform_binary_name("ffprobe");
    let ffmpeg = find_file_named(extract_dir, &ffmpeg_name)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "ffmpeg missing from archive"))?;
    let ffprobe = find_file_named(extract_dir, &ffprobe_name)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "ffprobe missing from archive"))?;

    tokio::fs::copy(ffmpeg, runtime_binary_path("ffmpeg")).await?;
    tokio::fs::copy(ffprobe, runtime_binary_path("ffprobe")).await?;
    make_executable(&runtime_binary_path("ffmpeg"))?;
    make_executable(&runtime_binary_path("ffprobe"))?;
    Ok(())
}

async fn extract_zip_binaries(archive: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use async_zip::base::read::stream::ZipFileReader;
    use futures_lite::io::AsyncReadExt;
    use tokio::io::{AsyncWriteExt, BufReader};

    let f = tokio::fs::File::open(archive).await?;
    let mut zip = ZipFileReader::with_tokio(BufReader::new(f));
    let ffmpeg_name = platform_binary_name("ffmpeg").to_lowercase();
    let ffprobe_name = platform_binary_name("ffprobe").to_lowercase();
    let mut ffmpeg_found = false;
    let mut ffprobe_found = false;

    loop {
        let mut entry = match zip.next_with_entry().await? {
            Some(e) => e,
            None => break,
        };
        let filename = entry.reader().entry().filename().as_str()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("zip filename: {}", e)))?
            .replace('\\', "/");
        let leaf = filename.rsplit('/').next().unwrap_or("").to_lowercase();

        if leaf == ffmpeg_name || leaf == ffprobe_name {
            let mut data = Vec::new();
            entry.reader_mut().read_to_end(&mut data).await?;
            let target = if leaf == ffmpeg_name {
                ffmpeg_found = true;
                runtime_binary_path("ffmpeg")
            } else {
                ffprobe_found = true;
                runtime_binary_path("ffprobe")
            };
            let mut out = tokio::fs::File::create(&target).await?;
            out.write_all(&data).await?;
            out.sync_all().await?;
            make_executable(&target)?;
        }

        zip = entry.skip().await?;
    }

    if !ffmpeg_found || !ffprobe_found {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "ffmpeg or ffprobe missing from zip archive").into());
    }
    Ok(())
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
