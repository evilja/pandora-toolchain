use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Ok(output) = Command::new("git").args(["symbolic-ref", "HEAD"]).output() {
        if output.status.success() {
            if let Ok(reference) = String::from_utf8(output.stdout) {
                println!("cargo:rerun-if-changed=.git/{}", reference.trim());
            }
        }
    }
    println!("cargo:rerun-if-env-changed=PANDORA_SOURCE_COMMIT");
    let commit = std::env::var("PANDORA_SOURCE_COMMIT").ok().filter(|value| !value.is_empty()).unwrap_or_else(|| Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_default());
    let commit = commit.trim();
    let commit = if commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit()) { commit } else { "" };
    println!("cargo:rustc-env=PANDORA_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=PANDORA_TARGET_FEATURES={}", std::env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default());
}
