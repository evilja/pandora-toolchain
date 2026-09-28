use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::pnworker::link::spec::ReleaseInfo;

pub const NAMES: [&str; 5] = ["pndc", "pnmpeg", "pnp2p", "pncurl", "pnass"];
const ROOT: &str = "DB/bin/pandora";
const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryFile {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryBundle {
    pub commit: String,
    pub target: String,
    pub glibc: String,
    pub cpu_features: Vec<String>,
    pub encoder_identity: String,
    pub files: Vec<BinaryFile>,
}

pub fn compiled_commit() -> &'static str {
    env!("PANDORA_BUILD_COMMIT")
}

pub fn print_binary_info_if_requested() -> bool {
    if !std::env::args().any(|arg| arg == "--link-binary-info") { return false; }
    println!("{}", serde_json::json!({
        "commit": compiled_commit(),
        "encoder_identity": crate::pnworker::link::client::encoder_identity(),
    }));
    true
}

fn glibc_version() -> Option<(u32, u32)> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let output = std::process::Command::new("getconf")
        .arg("GNU_LIBC_VERSION")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let version = value.trim().strip_prefix("glibc ")?;
    let (major, minor) = version.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn cpu_features() -> Vec<String> {
    env!("PANDORA_TARGET_FEATURES")
        .split(',')
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn has_cpu_feature(feature: &str) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        match feature {
            "fxsr" | "sse" => true, // Guaranteed by the x86_64 baseline.
            "sse2" => std::is_x86_feature_detected!("sse2"),
            "sse3" => std::is_x86_feature_detected!("sse3"),
            "ssse3" => std::is_x86_feature_detected!("ssse3"),
            "sse4.1" => std::is_x86_feature_detected!("sse4.1"),
            "sse4.2" => std::is_x86_feature_detected!("sse4.2"),
            "avx" => std::is_x86_feature_detected!("avx"),
            "avx2" => std::is_x86_feature_detected!("avx2"),
            "fma" => std::is_x86_feature_detected!("fma"),
            "bmi1" => std::is_x86_feature_detected!("bmi1"),
            "bmi2" => std::is_x86_feature_detected!("bmi2"),
            _ => false,
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = feature;
        false
    }
}

fn hash_file(path: &Path) -> Result<BinaryFile, String> {
    use std::io::Read;
    let name = path.file_name().ok_or("binary has no name")?.to_string_lossy().to_string();
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 { break; }
        bytes += read as u64;
        if bytes > MAX_BINARY_BYTES { return Err(format!("{name} exceeds the package limit")); }
        hash.update(&buffer[..read]);
    }
    Ok(BinaryFile { name, bytes, sha256: format!("{:x}", hash.finalize()) })
}

fn runtime_directory() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

// Only publish the running build, never binaries guessed from a newly pulled checkout.
pub fn published(commit: &str) -> Option<BinaryBundle> {
    static BUNDLE: OnceLock<Option<BinaryBundle>> = OnceLock::new();
    if !cfg!(target_arch = "x86_64") || commit.is_empty() || commit != compiled_commit() { return None; }
    BUNDLE.get_or_init(|| {
        let dir = runtime_directory()?;
        let (major, minor) = glibc_version()?;
        let identity = crate::pnworker::link::client::encoder_identity();
        for name in NAMES {
            let output = std::process::Command::new(dir.join(name))
                .arg("--link-binary-info").output().ok()?;
            if !output.status.success() { return None; }
            let info: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
            if info["commit"] != compiled_commit() || info["encoder_identity"] != identity { return None; }
        }
        let files = NAMES.iter().map(|name| hash_file(&dir.join(name)).ok()).collect::<Option<Vec<_>>>()?;
        Some(BinaryBundle {
            commit: compiled_commit().to_string(),
            target: target(),
            glibc: format!("{major}.{minor}"),
            cpu_features: cpu_features(),
            encoder_identity: identity,
            files,
        })
    }).clone()
}

pub fn published_file(commit: &str, name: &str, digest: &str) -> Option<PathBuf> {
    let bundle = published(commit)?;
    if !bundle.files.iter().any(|file| file.name == name && file.sha256 == digest) { return None; }
    Some(runtime_directory()?.join(name))
}

pub fn compatibility(bundle: &BinaryBundle) -> Result<(), String> {
    if bundle.target != target() { return Err(format!("target {} differs from {}", bundle.target, target())); }
    let (major, minor) = glibc_version().ok_or("this node has no supported glibc runtime")?;
    let (required_major, required_minor) = bundle.glibc.split_once('.')
        .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)))
        .ok_or("invalid package glibc version")?;
    if (major, minor) < (required_major, required_minor) {
        return Err(format!("glibc {major}.{minor} is older than required {required_major}.{required_minor}"));
    }
    for feature in &bundle.cpu_features {
        if !has_cpu_feature(feature) { return Err(format!("CPU lacks required {feature}")); }
    }
    if bundle.files.len() != NAMES.len() || NAMES.iter().any(|name| !bundle.files.iter().any(|file| file.name == *name)) {
        return Err("package does not contain the five required executables".to_string());
    }
    if bundle.files.iter().any(|file| !NAMES.contains(&file.name.as_str()) || file.bytes == 0 || file.bytes > MAX_BINARY_BYTES || file.sha256.len() != 64) {
        return Err("package contains an invalid file descriptor".to_string());
    }
    Ok(())
}

pub fn active_binary_matches(release: &ReleaseInfo) -> bool {
    let Some(bundle) = &release.binaries else { return false; };
    if crate::pnworker::link::client::encoder_identity() != bundle.encoder_identity { return false; }
    let current = std::env::current_exe().ok();
    let installed = std::fs::canonicalize(Path::new(ROOT).join("current/pndc")).ok();
    current.zip(installed).is_some_and(|(running, selected)| running == selected)
        && crate::lib::release::read().is_level_with(release.build, &release.commit)
}

#[derive(Serialize, Deserialize)]
struct PendingActivation {
    build: u64,
    commit: String,
    encoder_identity: String,
}

pub fn complete_pending() -> Result<(), String> {
    let path = Path::new(ROOT).join("pending.json");
    if !path.exists() { return Ok(()); }
    let pending: PendingActivation = serde_json::from_slice(&std::fs::read(&path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let installed = std::fs::canonicalize(Path::new(ROOT).join("current/pndc")).map_err(|error| error.to_string())?;
    if current != installed || compiled_commit() != pending.commit || crate::pnworker::link::client::encoder_identity() != pending.encoder_identity {
        return Err("running process does not match the pending binary release".to_string());
    }
    crate::lib::release::write(&crate::lib::release::ReleaseRecord { build: pending.build, commit: pending.commit })
        .map_err(|error| error.to_string())?;
    std::fs::remove_file(path).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(unix)]
pub async fn install(
    client: &reqwest::Client,
    coordinator: &str,
    token: &str,
    release: &ReleaseInfo,
    bundle: &BinaryBundle,
) -> Result<(), String> {
    use std::os::unix::fs::{symlink, PermissionsExt};
    compatibility(bundle)?;
    if bundle.commit != release.commit { return Err("package commit differs from release".to_string()); }
    let root = PathBuf::from(ROOT);
    let releases = root.join("releases");
    tokio::fs::create_dir_all(&releases).await.map_err(|error| error.to_string())?;
    let tag = format!("{}-{}", release.build, release.commit);
    let staging = releases.join(format!(".staging-{}-{}", tag, std::process::id()));
    if staging.exists() { tokio::fs::remove_dir_all(&staging).await.map_err(|error| error.to_string())?; }
    tokio::fs::create_dir(&staging).await.map_err(|error| error.to_string())?;
    let result = async {
        for entry in &bundle.files {
            let response = client.get(format!("{coordinator}/api/v1/link/binaries/{}/{}", entry.sha256, entry.name))
                .bearer_auth(token)
                .timeout(std::time::Duration::from_secs(300))
                .send().await.map_err(|error| error.to_string())?;
            if !response.status().is_success() { return Err(format!("{} download returned {}", entry.name, response.status())); }
            let path = staging.join(&entry.name);
            let mut file = tokio::fs::File::create(&path).await.map_err(|error| error.to_string())?;
            let mut stream = response.bytes_stream();
            use futures_lite::StreamExt;
            let mut hash = Sha256::new();
            let mut size = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| error.to_string())?;
                size += chunk.len() as u64;
                if size > entry.bytes { return Err(format!("{} exceeds its declared size", entry.name)); }
                hash.update(&chunk);
                file.write_all(&chunk).await.map_err(|error| error.to_string())?;
            }
            file.sync_all().await.map_err(|error| error.to_string())?;
            if size != entry.bytes || format!("{:x}", hash.finalize()) != entry.sha256 {
                return Err(format!("{} failed size or SHA-256 verification", entry.name));
            }
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .await.map_err(|error| error.to_string())?;
        }
        for name in NAMES {
            let probe = tokio::process::Command::new(staging.join(name))
                .arg("--link-binary-info").output().await.map_err(|error| error.to_string())?;
            if !probe.status.success() { return Err(format!("downloaded {name} cannot start on this host")); }
            let info: serde_json::Value = serde_json::from_slice(&probe.stdout).map_err(|error| error.to_string())?;
            if info["commit"] != release.commit || info["encoder_identity"] != bundle.encoder_identity {
                return Err(format!("downloaded {name} build or encoder identity differs from manifest"));
            }
        }
        let destination = releases.join(&tag);
        if destination.exists() {
            let matches = bundle.files.iter().all(|entry| {
                hash_file(&destination.join(&entry.name)).is_ok_and(|actual| actual == *entry)
            });
            if !matches { return Err(format!("existing package {} is incomplete or differs from the manifest", destination.display())); }
            tokio::fs::remove_dir_all(&staging).await.map_err(|error| error.to_string())?;
        } else {
            tokio::fs::rename(&staging, &destination).await.map_err(|error| error.to_string())?;
        }
        let previous = root.join("previous");
        let current = root.join("current");
        if current.exists() {
            let old_target = tokio::fs::read_link(&current).await.map_err(|error| error.to_string())?;
            let previous_next = root.join("previous.next");
            if previous_next.exists() { tokio::fs::remove_file(&previous_next).await.map_err(|error| error.to_string())?; }
            symlink(old_target, &previous_next).map_err(|error| error.to_string())?;
            tokio::fs::rename(&previous_next, previous).await.map_err(|error| error.to_string())?;
        }
        let pending = PendingActivation {
            build: release.build,
            commit: release.commit.clone(),
            encoder_identity: bundle.encoder_identity.clone(),
        };
        tokio::fs::write(root.join("pending.json"), serde_json::to_vec(&pending).map_err(|error| error.to_string())?)
            .await.map_err(|error| error.to_string())?;
        let next = root.join("current.next");
        if next.exists() { tokio::fs::remove_file(&next).await.map_err(|error| error.to_string())?; }
        symlink(Path::new("releases").join(&tag), &next).map_err(|error| error.to_string())?;
        tokio::fs::rename(&next, &current).await.map_err(|error| error.to_string())?;
        let _ = tokio::fs::remove_file(root.join("source-active")).await;
        let _ = tokio::fs::remove_file(root.join("source-build.request")).await;
        Ok(())
    }.await;
    if result.is_err() { let _ = tokio::fs::remove_dir_all(&staging).await; }
    result
}

#[cfg(not(unix))]
pub async fn install(_: &reqwest::Client, _: &str, _: &str, _: &ReleaseInfo, _: &BinaryBundle) -> Result<(), String> {
    Err("binary installation is not supported on this platform".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_hash_is_sha256_of_the_exact_file() {
        let path = std::env::temp_dir().join(format!("pandora-binary-hash-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        let entry = hash_file(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(entry.bytes, 3);
        assert_eq!(entry.sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn old_release_response_can_omit_binary_manifest() {
        let release: ReleaseInfo = serde_json::from_str(
            r#"{"version":"4.0.0-chiri","build":7,"commit":"abc","reset":false}"#,
        ).unwrap();
        assert!(release.binaries.is_none());
    }
}
