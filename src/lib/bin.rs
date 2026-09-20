use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::lib::env::core::{get_pandora_env, upsert_env};
use crate::lib::env::standard::{ENV_PATH, FFMPEG_BUILD, PNASS, PNCURL, PNMPEG, PNP2P};

enum ArchiveKind {
    TarXz,
    Zip,
}

// The build script, carried inside the binary so `pndc --build-ffmpeg` and `/build-ffmpeg` work
// on a machine that only has the binary — a Docker runtime image, a node that was handed `pndc`
// and nothing else. It is written out to `DB/bin/build/` and run from there; the copy in
// `scripts/` is the one to edit.
const FFMPEG_BUILD_SCRIPT: &str = include_str!("../../scripts/build-ffmpeg.sh");
const FFMPEG_BUILD_DIR: &str = "DB/bin/build";
const FFMPEG_BUILD_LOG: &str = "DB/bin/build/build-ffmpeg.log";
// Written by the script beside the binaries it installed. Its presence is what separates a
// native build from a downloaded one, which is the only thing startup needs to know about it.
const FFMPEG_BUILD_RECORD: &str = "DB/bin/ffmpeg.build";
// The same record for a pair baked into a Docker image (`FFMPEG_NATIVE=1`): those binaries are
// on PATH rather than in `DB/bin`, and this is how startup still knows what they are.
const IMAGE_FFMPEG_BUILD_RECORD: &str = "/usr/local/share/pandora/ffmpeg.build";

static FFMPEG_BUILD_RUNNING: AtomicBool = AtomicBool::new(false);

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

// Whether `env.pandora` asks for ffmpeg to be compiled here rather than downloaded:
// `ffmpeg_build|pntools|native`. Anything else — including the key being absent — keeps the
// portable download, so an existing deployment is not turned into a twenty-minute compile by an
// upgrade.
pub fn native_ffmpeg_requested() -> bool {
    get_pandora_env()
        .get(FFMPEG_BUILD)
        .map(|value| value.trim().eq_ignore_ascii_case("native"))
        .unwrap_or(false)
}

// The record the build script leaves beside a native ffmpeg, or `None` for a downloaded one.
pub fn native_ffmpeg_record() -> Option<String> {
    std::fs::read_to_string(FFMPEG_BUILD_RECORD)
        .ok()
        .filter(|record| !record.trim().is_empty())
}

pub fn native_ffmpeg_build_running() -> bool {
    FFMPEG_BUILD_RUNNING.load(Ordering::SeqCst)
}

// Why a build cannot even start here, known without running the script. The one case worth
// telling apart is a container with no compiler: the script's answer to that is four
// package-manager lines, none of which means anything inside an image, and the image usually
// carries a native pair already — which is what whoever ran the command was after.
pub enum NativeBuildBlocker {
    // No compiler, and the ffmpeg in use is already a native build. `record` is the path of the
    // build record describing it: `DB/bin`'s own, or the one baked into the image.
    NoCompilerNativeInUse { built_at: String, tuning: String, record: String },
    // No compiler, the image carries a native pair, and a downloaded pair in `DB/bin` wins over it.
    NoCompilerNativeShadowed,
    // No compiler and no native pair anywhere.
    NoCompiler,
}

pub fn native_build_blocker() -> Option<NativeBuildBlocker> {
    let image_record = std::fs::read_to_string(IMAGE_FFMPEG_BUILD_RECORD)
        .ok()
        .filter(|record| !record.trim().is_empty());
    let in_container = image_record.is_some() || Path::new("/.dockerenv").exists();
    if !in_container || on_path("cc") || on_path("gcc") || on_path("clang") {
        return None;
    }
    let local = local_binary_available("ffmpeg") && local_binary_available("ffprobe");
    let in_use = if local {
        native_ffmpeg_record().map(|record| (record, FFMPEG_BUILD_RECORD))
    } else {
        image_record.clone().map(|record| (record, IMAGE_FFMPEG_BUILD_RECORD))
    };
    Some(match in_use {
        Some((record, path)) => NativeBuildBlocker::NoCompilerNativeInUse {
            built_at: build_record_value(&record, "built_at"),
            tuning: build_record_value(&record, "tuning"),
            record: path.to_string(),
        },
        None if local && image_record.is_some() => NativeBuildBlocker::NoCompilerNativeShadowed,
        None => NativeBuildBlocker::NoCompiler,
    })
}

fn build_record_value(record: &str, key: &str) -> String {
    record
        .lines()
        .find_map(|line| line.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "?".to_string())
}

// Whether PATH holds an executable of that name. `command_available` cannot answer this for a
// compiler: it runs `-version`, which ffmpeg understands and `cc` exits non-zero on.
fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

// One milestone of a running build: which of the script's steps it belongs to, and the line the
// script wrote. The script reports no percentage — a compile has none to give — so the step is
// the only measure of how far along it is.
pub struct FfmpegBuildProgress {
    pub step: u8,
    pub line: String,
}

pub const FFMPEG_BUILD_STEPS: u8 = 5;

// The step a line of the script's own narrative belongs to: x264, x265, libass, ffmpeg (with the
// NVENC headers it is configured against), then the smoke test and install. `None` for the
// header lines before anything is built and for anything the script did not write itself — a
// compiler's output in a failure tail names these libraries too and is not a milestone.
pub fn ffmpeg_build_step(line: &str) -> Option<(u8, &str)> {
    let text = line.trim().strip_prefix("[build-ffmpeg]")?.trim();
    if ["host:", "tuning:", "versions:", "output:", "cleaning "]
        .iter()
        .any(|header| text.starts_with(header))
    {
        return None;
    }
    let step = if text.starts_with("smoke test") || text.starts_with("installed ") || text.starts_with("record:") {
        5
    } else if text.contains("nv-codec-headers") || text.contains(" ffmpeg") {
        4
    } else if text.contains("libass") {
        3
    } else if text.contains("x265") {
        2
    } else if text.contains("x264") {
        1
    } else {
        return None;
    };
    Some((step, text))
}

pub struct NativeFfmpegBuild {
    // The first line of `ffmpeg -version` from the pair that was installed.
    pub version: String,
    pub elapsed: Duration,
    pub log: PathBuf,
}

// Runs the embedded build script, streaming its output to `DB/bin/build/build-ffmpeg.log` and
// to this process's stdout. One build at a time: a second caller is told the first is running
// rather than being queued behind twenty minutes of compiling it did not know about. `clean`
// throws the work tree away first; without it a rebuild after a version bump only redoes the
// component that changed. `progress` is handed each milestone as the script reaches it, for a
// caller with somewhere to show it; the log and stdout get every line either way.
pub async fn build_native_ffmpeg(
    clean: bool,
    progress: Option<tokio::sync::mpsc::UnboundedSender<FfmpegBuildProgress>>,
) -> Result<NativeFfmpegBuild, String> {
    if cfg!(windows) {
        return Err("building ffmpeg natively is not supported on Windows; the portable download stays in use".to_string());
    }
    if FFMPEG_BUILD_RUNNING.swap(true, Ordering::SeqCst) {
        return Err(format!("a native ffmpeg build is already running; its log is {}", FFMPEG_BUILD_LOG));
    }
    let result = run_native_ffmpeg_build(clean, progress).await;
    FFMPEG_BUILD_RUNNING.store(false, Ordering::SeqCst);
    result
}

async fn run_native_ffmpeg_build(
    clean: bool,
    progress: Option<tokio::sync::mpsc::UnboundedSender<FfmpegBuildProgress>>,
) -> Result<NativeFfmpegBuild, String> {
    use tokio::io::{AsyncWriteExt, BufReader};

    tokio::fs::create_dir_all(FFMPEG_BUILD_DIR)
        .await
        .map_err(|e| format!("failed to create {}: {}", FFMPEG_BUILD_DIR, e))?;
    let script = PathBuf::from(FFMPEG_BUILD_DIR).join("build-ffmpeg.sh");
    tokio::fs::write(&script, FFMPEG_BUILD_SCRIPT)
        .await
        .map_err(|e| format!("failed to write {}: {}", script.display(), e))?;
    make_executable(&script).map_err(|e| format!("failed to mark {} executable: {}", script.display(), e))?;

    // The script defaults to `DB/bin` relative to its cwd, which is this process's cwd too; the
    // absolute path is passed anyway so the record it writes names where the binaries really are.
    let out_dir = std::env::current_dir()
        .map_err(|e| format!("cannot read the working directory: {}", e))?
        .join("DB")
        .join("bin");
    let mut log = tokio::fs::File::create(FFMPEG_BUILD_LOG)
        .await
        .map_err(|e| format!("failed to create {}: {}", FFMPEG_BUILD_LOG, e))?;

    let started = Instant::now();
    println!("[ffmpeg-build] building ffmpeg natively into {} (log: {})", out_dir.display(), FFMPEG_BUILD_LOG);
    let mut child = tokio::process::Command::new("bash")
        .arg(&script)
        .env("PANDORA_FFMPEG_OUT", &out_dir)
        .env("PANDORA_FFMPEG_CLEAN", if clean { "1" } else { "0" })
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start bash for the build script: {}", e))?;

    // Both streams go through one writer so the log reads in the order things happened; the
    // script itself keeps each component's full compiler output in its own file under the work
    // tree, so this one stays a narrative rather than ten thousand compiler lines.
    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(256);
    let stdout = child.stdout.take().ok_or("build script stdout was not captured")?;
    let stderr = child.stderr.take().ok_or("build script stderr was not captured")?;
    for reader in [
        tokio::task::spawn(pump_lines(BufReader::new(stdout), line_tx.clone())),
        tokio::task::spawn(pump_lines(BufReader::new(stderr), line_tx.clone())),
    ] {
        drop(reader);
    }
    drop(line_tx);
    let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::with_capacity(12);
    while let Some(line) = line_rx.recv().await {
        println!("[ffmpeg-build] {}", line);
        let _ = log.write_all(line.as_bytes()).await;
        let _ = log.write_all(b"\n").await;
        if let Some(progress) = &progress {
            let plain = strip_ansi(line.clone());
            if let Some((step, text)) = ffmpeg_build_step(&plain) {
                let _ = progress.send(FfmpegBuildProgress { step, line: text.to_string() });
            }
        }
        if tail.len() == 12 {
            tail.pop_front();
        }
        tail.push_back(line);
    }
    let _ = log.flush().await;
    let status = child
        .wait()
        .await
        .map_err(|e| format!("failed to wait for the build script: {}", e))?;
    if !status.success() {
        let tail: Vec<String> = tail.into_iter().map(strip_ansi).collect();
        return Err(format!(
            "the build script exited with {} after {}m; the end of its log:\n{}",
            status.code().map(|c| c.to_string()).unwrap_or_else(|| "a signal".to_string()),
            started.elapsed().as_secs() / 60,
            tail.join("\n"),
        ));
    }
    if !(local_binary_available("ffmpeg") && local_binary_available("ffprobe")) {
        return Err("the build script finished but DB/bin/ffmpeg or DB/bin/ffprobe does not run".to_string());
    }
    let version = binary_version_line(&runtime_binary_path("ffmpeg")).unwrap_or_else(|| "ffmpeg (version unknown)".to_string());
    println!("[ffmpeg-build] done in {}m: {}", started.elapsed().as_secs() / 60, version);
    Ok(NativeFfmpegBuild {
        version,
        elapsed: started.elapsed(),
        log: PathBuf::from(FFMPEG_BUILD_LOG),
    })
}

async fn pump_lines<R>(reader: tokio::io::BufReader<R>, tx: tokio::sync::mpsc::Sender<String>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if tx.send(line).await.is_err() {
            break;
        }
    }
}

// The script colours its own lines for a terminal; in a Discord reply the escapes are noise.
fn strip_ansi(line: String) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn binary_version_line(path: &Path) -> Option<String> {
    let output = Command::new(path).arg("-version").stdin(Stdio::null()).output().ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
}

fn print_build_record(record: &str) {
    for line in record.lines().filter(|line| {
        line.starts_with("built_at=") || line.starts_with("cpu=") || line.starts_with("tuning=") || line.starts_with("ffmpeg=")
    }) {
        println!("Runtime binary check:   {}", line);
    }
}

pub async fn ensure_startup_binaries() {
    if let Err(e) = tokio::fs::create_dir_all("DB/bin").await {
        eprintln!("Warning: failed to create DB/bin: {}", e);
        return;
    }

    ensure_tool_env_paths();

    let native_requested = native_ffmpeg_requested();
    let local = local_binary_available("ffmpeg") && local_binary_available("ffprobe");

    // `DB/bin` is checked before PATH because `resolve_runtime_binary` prefers it: reporting the
    // PATH copy while every tool then runs the local one would describe a binary nobody uses.
    if local {
        match native_ffmpeg_record() {
            Some(record) => {
                println!("Runtime binary check: ffmpeg and ffprobe are the native build in DB/bin");
                print_build_record(&record);
                return;
            }
            None if native_requested => {
                // A downloaded pair sitting where a native one was asked for: build over it.
                println!("Runtime binary check: DB/bin holds a downloaded ffmpeg but {} asks for a native build", FFMPEG_BUILD);
            }
            None => {
                println!("Runtime binary check: ffmpeg and ffprobe found in DB/bin");
                return;
            }
        }
    } else if command_available("ffmpeg") && command_available("ffprobe") && !native_requested {
        match std::fs::read_to_string(IMAGE_FFMPEG_BUILD_RECORD) {
            Ok(record) if !record.trim().is_empty() => {
                println!("Runtime binary check: ffmpeg and ffprobe are the native build baked into this image");
                print_build_record(&record);
            }
            _ => println!("Runtime binary check: ffmpeg and ffprobe found in PATH"),
        }
        return;
    }

    if native_requested {
        println!(
            "Runtime binary check: building ffmpeg natively for this CPU ({}={}); this takes a while and Pandora starts when it is done",
            FFMPEG_BUILD, "native"
        );
        match build_native_ffmpeg(false, None).await {
            Ok(build) => println!("Runtime binary check: native ffmpeg installed in DB/bin ({})", build.version),
            Err(e) => {
                eprintln!("Warning: the native ffmpeg build failed: {}", e);
                if local || (command_available("ffmpeg") && command_available("ffprobe")) {
                    eprintln!("Warning: continuing with the ffmpeg that was already here; fix the build and run `pndc --build-ffmpeg`");
                    return;
                }
                eprintln!("Warning: falling back to the portable download so Pandora can start at all");
            }
        }
        if local_binary_available("ffmpeg") && local_binary_available("ffprobe") {
            return;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_script_narrative_maps_onto_its_steps() {
        for (line, step) in [
            ("[build-ffmpeg] cloning x264 (stable) from https://code.videolan.org/videolan/x264.git", 1),
            ("[build-ffmpeg] x264 stable already built", 1),
            ("[build-ffmpeg] building x265 (12-bit, 10-bit, then 8-bit with both linked in)", 2),
            ("[build-ffmpeg] downloading libass from https://github.com/libass/libass/releases/x.tar.xz", 3),
            ("[build-ffmpeg] installing nv-codec-headers", 4),
            ("[build-ffmpeg] downloading ffmpeg from https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz", 4),
            ("[build-ffmpeg] configuring ffmpeg: --prefix=/x --enable-libx264 --enable-libx265", 4),
            ("[build-ffmpeg] building ffmpeg with 16 jobs", 4),
            ("[build-ffmpeg] smoke test: libx265 main10", 5),
            ("[build-ffmpeg] installed /app/DB/bin/ffmpeg and /app/DB/bin/ffprobe", 5),
        ] {
            assert_eq!(ffmpeg_build_step(line).map(|(step, _)| step), Some(step), "{}", line);
        }
    }

    #[test]
    fn headers_and_compiler_output_are_not_milestones() {
        for line in [
            "[build-ffmpeg] versions: ffmpeg 8.1.2, x264 stable, x265 4.1, libass 0.17.5",
            "[build-ffmpeg] output: /app/DB/bin, work tree: /app/DB/bin/build",
            "[build-ffmpeg] missing tools: cc c++ make cmake pkg-config xz git nasm",
            "libavcodec/libx264.c:123: error: something about x264",
        ] {
            assert!(ffmpeg_build_step(line).is_none(), "{}", line);
        }
    }

    #[test]
    fn a_build_record_gives_up_its_values() {
        let record = "built_at=2026-09-01T10:00:00Z\ncpu=Some CPU\ntuning=-march=native\n";
        assert_eq!(build_record_value(record, "built_at"), "2026-09-01T10:00:00Z");
        assert_eq!(build_record_value(record, "tuning"), "-march=native");
        assert_eq!(build_record_value(record, "lto"), "?");
    }
}
