use std::collections::HashMap;
use std::io::IsTerminal;

use crate::lib::env::core::{get_pandora_env, upsert_env};
use crate::lib::env::standard::{
    API_HOST, API_PORT, API_PUBLIC_URL, ENV_PATH, ENV_SEP, LINK_COORDINATOR_URL, LINK_MAX_JOBS,
    LINK_NODE_NAME, LINK_NODE_TOKEN, LUMIERE_BROKER_TOKEN, LUMIERE_BROKER_URL, LUMIERE_PUBLIC_URL,
    PANDORA_MODE, TOKEN,
};

// First run. Nothing has ever created `env.pandora` — the migration at startup only moves one that
// already exists — so a fresh install used to reach serenity with an empty token and fail with a
// library error that named nothing an operator could act on. Under `start.sh`'s restart loop that
// is an unattended spin.
//
// Two rules shape everything here. A prompt must never appear where nobody can answer it: Docker
// runs `pndc` with no TTY, so without a terminal this writes a documented template and exits
// instead of blocking a deploy forever. And an existing install must never be interrupted: the
// automatic path triggers only on the keys a process genuinely cannot start without.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Coordinator,
    Node,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Coordinator => "coordinator",
            Role::Node => "Pandora Mini node",
        }
    }
}

pub struct SetupKey {
    pub key: &'static str,
    pub prompt: &'static str,
    pub help: &'static str,
    pub default: Option<&'static str>,
    // Never echoed back, and never printed in a summary.
    pub secret: bool,
    // Whether the process can start at all without it. Only these trigger setup on their own; the
    // rest are asked once you are already in it.
    pub required: bool,
}

const COORDINATOR_KEYS: &[SetupKey] = &[
    SetupKey {
        key: TOKEN,
        prompt: "Discord bot token",
        help: "From the Discord developer portal, Bot → Token. Without it there is no bot.",
        default: None,
        secret: true,
        required: true,
    },
    SetupKey {
        key: API_PORT,
        prompt: "HTTP API port",
        help: "Enables the API and web consoles. 0 disables them.",
        default: Some("8787"),
        secret: false,
        required: false,
    },
    SetupKey {
        key: API_HOST,
        prompt: "HTTP API bind address",
        help: "127.0.0.1 keeps it loopback-only behind a proxy; 0.0.0.0 listens everywhere.",
        default: Some("0.0.0.0"),
        secret: false,
        required: false,
    },
    SetupKey {
        key: API_PUBLIC_URL,
        prompt: "Public origin this instance is reachable on",
        help: "The tunnel hostname, no trailing slash. Batch pages, HLS playback and linked nodes \
               all address this instance through it.",
        default: None,
        secret: false,
        required: false,
    },
    SetupKey {
        key: LUMIERE_BROKER_URL,
        prompt: "Lumiere broker URL",
        help: "The Cloudflare Worker that holds the upload credentials. Without it an encode \
               finishes and then fails at upload.",
        default: None,
        secret: false,
        required: false,
    },
    SetupKey {
        key: LUMIERE_BROKER_TOKEN,
        prompt: "Lumiere broker token",
        help: "The scoped token for that Worker. It is the only upload credential this host holds.",
        default: None,
        secret: true,
        required: false,
    },
    SetupKey {
        key: LUMIERE_PUBLIC_URL,
        prompt: "Public origin providers fetch uploads from",
        help: "Usually the same as the public origin above; streaming hosts pull their copy from it.",
        default: None,
        secret: false,
        required: false,
    },
];

const NODE_KEYS: &[SetupKey] = &[
    SetupKey {
        key: LINK_COORDINATOR_URL,
        prompt: "Coordinator URL",
        help: "The public origin of the pndc this node takes work from, no trailing slash.",
        default: None,
        secret: false,
        required: true,
    },
    SetupKey {
        key: LINK_NODE_NAME,
        prompt: "Node name",
        help: "Must match the name its token was minted under with `/gentoken link:<node>`.",
        default: None,
        secret: false,
        required: true,
    },
    SetupKey {
        key: LINK_NODE_TOKEN,
        prompt: "Node token",
        help: "The `|link|` token from `/gentoken link:<node>` on the coordinator.",
        default: None,
        secret: true,
        required: true,
    },
    SetupKey {
        key: LINK_MAX_JOBS,
        prompt: "Concurrent jobs",
        help: "How many leases this node holds at once.",
        default: Some("1"),
        secret: false,
        required: false,
    },
];

pub fn keys(role: Role) -> &'static [SetupKey] {
    match role {
        Role::Coordinator => COORDINATOR_KEYS,
        Role::Node => NODE_KEYS,
    }
}

// Only the keys a process cannot start without. An install that has run for a year with no Lumiere
// configuration must not be dragged into a wizard by an upgrade.
pub fn missing_required(role: Role, env: &HashMap<String, String>) -> Vec<&'static SetupKey> {
    keys(role)
        .iter()
        .filter(|entry| entry.required)
        .filter(|entry| env.get(entry.key).map(|v| v.trim().is_empty()).unwrap_or(true))
        .collect()
}

// The role this process is about to run as, read from the command line and the config rather than
// through `link::client::is_mini`, whose answer is cached for the process — and setup may be about
// to write the very value that decides it.
pub fn detect_role(env: &HashMap<String, String>) -> Role {
    if std::env::args().any(|arg| arg == "--mini") {
        return Role::Node;
    }
    match env.get(PANDORA_MODE).map(|v| v.trim().to_ascii_lowercase()) {
        Some(mode) if mode == "mini" => Role::Node,
        _ => Role::Coordinator,
    }
}

pub fn wants_setup() -> bool {
    std::env::args().any(|arg| arg == "--setup")
}

// A commented `env.pandora` for an operator to fill in by hand. `get_env` already skips `#` lines
// and blank ones, so the file this writes is valid the moment it is saved.
pub fn template(role: Role) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Pandora {} configuration.\n# Each line is NAME{}VALUE. Lines starting with # are ignored.\n\n",
        role.label(),
        ENV_SEP
    ));
    if role == Role::Node {
        out.push_str("# This file configures a node; it takes work from a coordinator and runs no Discord bot.\n");
        out.push_str(&format!("{}{}mini\n\n", PANDORA_MODE, ENV_SEP));
    }
    for entry in keys(role) {
        out.push_str(&format!("# {}\n", entry.help));
        let value = entry.default.unwrap_or("");
        let marker = if entry.required { " (required)" } else { " (optional)" };
        out.push_str(&format!("# {}{}\n", entry.prompt, marker));
        out.push_str(&format!("{}{}{}\n\n", entry.key, ENV_SEP, value));
    }
    out
}

pub enum Outcome {
    // Configuration is present; carry on starting.
    Ready,
    // Nothing more can be done in this process — the reason has already been printed.
    Stop,
}

pub async fn ensure_configured() -> Outcome {
    // No `env.pandora` at all is the one unambiguous signal that this machine has never run
    // Pandora, and it is what separates a new install from a deployment that predates the ledger.
    // A new install is already in the current on-disk format — there is nothing for a migration to
    // convert — so it records them as done rather than running them. An older deployment reaches
    // its first sync with no ledger and runs every one, which is the whole point.
    if !std::path::Path::new(ENV_PATH).exists() {
        crate::lib::migration::seed(std::path::Path::new(&crate::lib::release::repo_path()));
    }
    let env = get_pandora_env();
    let role = detect_role(&env);
    let forced = wants_setup();
    let missing = missing_required(role, &env);
    if !forced && missing.is_empty() {
        return Outcome::Ready;
    }

    if !std::io::stdin().is_terminal() {
        return unattended(role, &missing);
    }
    run_wizard(role).await
}

// No terminal: a container, a service unit, `start.sh` in a loop. Blocking on a prompt here would
// hang a deploy with no indication of why, so this leaves behind something to edit and stops.
fn unattended(role: Role, missing: &[&SetupKey]) -> Outcome {
    println!("\n[setup] Pandora is not configured to run as a {}.", role.label());
    if !missing.is_empty() {
        println!("[setup] Missing required setting(s):");
        for entry in missing {
            println!("[setup]   {} — {}", entry.key, entry.help);
        }
    }
    let path = std::path::Path::new(ENV_PATH);
    if path.exists() {
        println!("[setup] Add them to {ENV_PATH} and start Pandora again.");
    } else {
        match std::fs::write(path, template(role)) {
            Ok(()) => {
                restrict(path);
                println!("[setup] Wrote a template to {ENV_PATH}. Fill it in and start Pandora again.");
            }
            Err(error) => {
                eprintln!("[setup] Could not write {ENV_PATH}: {error}");
            }
        }
    }
    println!("[setup] With a terminal attached, `pndc --setup` asks these questions interactively.\n");
    Outcome::Stop
}

#[cfg(unix)]
fn restrict(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) {}

async fn run_wizard(role: Role) -> Outcome {
    println!("\n── Pandora setup ──");
    println!("Configuring this instance as a {}.", role.label());
    println!("Answers are written to {ENV_PATH}. Press Enter to accept a [default], or leave blank to skip an optional setting.\n");

    // Created and restricted before the first answer, not after the last: the first token typed
    // would otherwise land in a file with default permissions and sit there for the rest of the
    // interview.
    let path = std::path::Path::new(ENV_PATH);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Err(error) = std::fs::write(path, "") {
            eprintln!("[setup] Could not create {ENV_PATH}: {error}");
            return Outcome::Stop;
        }
    }
    restrict(path);

    let mut answers: HashMap<&'static str, String> = HashMap::new();
    for entry in keys(role) {
        let existing = get_pandora_env().get(entry.key).cloned().unwrap_or_default();
        loop {
            let Some(value) = ask(entry, &existing).await else {
                println!("\n[setup] Cancelled; nothing was written.");
                return Outcome::Stop;
            };
            if value.trim().is_empty() {
                if entry.required {
                    println!("  {} is required.", entry.key);
                    continue;
                }
                break;
            }
            if let Err(reason) = validate(entry.key, value.trim()) {
                println!("  ✗ {reason}");
                continue;
            }
            answers.insert(entry.key, value.trim().to_string());
            if let Err(error) = upsert_env(ENV_PATH, entry.key, value.trim()) {
                eprintln!("  ✗ could not save {}: {error}", entry.key);
                return Outcome::Stop;
            }
            break;
        }
    }
    if role == Role::Node {
        upsert_env(ENV_PATH, PANDORA_MODE, "mini").ok();
    }

    println!("\n[setup] Checking what you entered…");
    let checks = verify(role, &answers).await;
    for (label, result) in &checks {
        match result {
            Ok(detail) => println!("  ✓ {label}: {detail}"),
            Err(reason) => println!("  ✗ {label}: {reason}"),
        }
    }
    let failed = checks.iter().filter(|(_, r)| r.is_err()).count();
    println!(
        "\n[setup] Saved to {ENV_PATH}.{}\n",
        if failed > 0 {
            format!(" {failed} check(s) did not pass — Pandora will start, but fix them if it misbehaves.")
        } else {
            String::new()
        }
    );
    Outcome::Ready
}

async fn ask(entry: &'static SetupKey, existing: &str) -> Option<String> {
    let shown_default = if !existing.is_empty() {
        Some(if entry.secret { "keep current".to_string() } else { existing.to_string() })
    } else {
        entry.default.map(|value| value.to_string())
    };
    let suffix = match &shown_default {
        Some(value) => format!(" [{value}]"),
        None if entry.required => " (required)".to_string(),
        None => " (optional, Enter to skip)".to_string(),
    };
    println!("\n{}", entry.help);
    let line = read_line(&format!("{}{}: ", entry.prompt, suffix)).await?;
    let line = line.trim().to_string();
    if !line.is_empty() {
        return Some(line);
    }
    // Enter on a key that already has a value keeps it, rather than clearing it.
    if !existing.is_empty() {
        return Some(existing.to_string());
    }
    Some(entry.default.unwrap_or("").to_string())
}

async fn read_line(prompt: &str) -> Option<String> {
    let prompt = prompt.to_string();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        print!("{prompt}");
        std::io::stdout().flush().ok();
        let mut buffer = String::new();
        match std::io::stdin().read_line(&mut buffer) {
            // End of input — the operator pressed Ctrl-D.
            Ok(0) => None,
            Ok(_) => Some(buffer),
            Err(_) => None,
        }
    })
    .await
    .ok()
    .flatten()
}

// Shape checks that need no network, so an obvious typo is caught before a request is made.
pub fn validate(key: &str, value: &str) -> Result<(), String> {
    match key {
        API_PORT => value
            .parse::<u16>()
            .map(|_| ())
            .map_err(|_| "must be a port number between 0 and 65535".to_string()),
        LINK_MAX_JOBS => match value.parse::<u32>() {
            Ok(n) if n >= 1 => Ok(()),
            _ => Err("must be a whole number of at least 1".to_string()),
        },
        API_PUBLIC_URL | LUMIERE_BROKER_URL | LUMIERE_PUBLIC_URL | LINK_COORDINATOR_URL => {
            if value.ends_with('/') {
                return Err("must not end with a trailing slash".to_string());
            }
            match reqwest::Url::parse(value) {
                Ok(url) if url.scheme() == "http" || url.scheme() == "https" => Ok(()),
                Ok(_) => Err("must be an http:// or https:// URL".to_string()),
                Err(error) => Err(format!("is not a URL ({error})")),
            }
        }
        LINK_NODE_NAME => {
            if value.contains('|') || value.chars().any(char::is_whitespace) {
                Err("must not contain spaces or `|`".to_string())
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

type Check = (String, Result<String, String>);

async fn verify(role: Role, answers: &HashMap<&'static str, String>) -> Vec<Check> {
    let env = get_pandora_env();
    let value = |key: &str| -> Option<String> {
        answers
            .get(key)
            .cloned()
            .or_else(|| env.get(key).cloned())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let mut checks = Vec::new();
    match role {
        Role::Coordinator => {
            if let Some(token) = value(TOKEN) {
                checks.push(("Discord token".to_string(), verify_discord(&token).await));
            }
            if let (Some(url), Some(token)) = (value(LUMIERE_BROKER_URL), value(LUMIERE_BROKER_TOKEN))
            {
                checks.push(("Lumiere broker".to_string(), verify_broker(&url, &token).await));
            }
        }
        Role::Node => {
            if let (Some(url), Some(token), Some(node)) = (
                value(LINK_COORDINATOR_URL),
                value(LINK_NODE_TOKEN),
                value(LINK_NODE_NAME),
            ) {
                checks.push(("Coordinator link".to_string(), verify_link(&url, &token, &node).await));
            }
        }
    }
    checks
}

async fn verify_discord(token: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://discord.com/api/v10/users/@me")
        .header("Authorization", format!("Bot {token}"))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|error| format!("could not reach Discord ({error})"))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err("Discord rejected this token".to_string());
    }
    if !response.status().is_success() {
        return Err(format!("Discord answered {}", response.status()));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("Discord sent an unreadable answer ({error})"))?;
    let name = body
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    Ok(format!("authenticated as {name}"))
}

async fn verify_broker(url: &str, token: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/v1/status", url.trim_end_matches('/')))
        .bearer_auth(token)
        .header("X-Lumiere-Version", crate::lumiere_broker::API_VERSION)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|error| format!("could not reach the broker ({error})"))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED
        || response.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Err("the broker rejected this token".to_string());
    }
    if !response.status().is_success() {
        return Err(format!("the broker answered {}", response.status()));
    }
    Ok("reachable and the token is accepted".to_string())
}

// Registering is the check, because it is the only call that tests every answer at once: the URL,
// the token, the node name it is bound to, and whether this build's encoder matches. A coordinator
// that refuses says exactly which of those was wrong.
async fn verify_link(url: &str, token: &str, node: &str) -> Result<String, String> {
    let body = crate::pnworker::link::spec::NodeRegister {
        node: node.to_string(),
        pandora_version: env!("CARGO_PKG_VERSION").to_string(),
        build: crate::lib::release::read().build,
        migration_error: crate::lib::migration::read_ledger()
            .failure
            .map(|failure| failure.line()),
        encoder_identity: crate::pnworker::link::client::encoder_identity(),
        ffmpeg_version: String::new(),
        threads: 1,
        max_jobs: 1,
        presets: Vec::new(),
    };
    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/link/register", url.trim_end_matches('/')))
        .bearer_auth(token)
        .json(&body)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|error| format!("could not reach the coordinator ({error})"))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err("the coordinator rejected this token".to_string());
    }
    if response.status() == reqwest::StatusCode::FORBIDDEN {
        // Carries the node name the token is actually bound to, which is the answer to the most
        // likely mistake.
        return Err(response.text().await.unwrap_or_else(|_| "forbidden".to_string()));
    }
    if !response.status().is_success() {
        return Err(format!("the coordinator answered {}", response.status()));
    }
    let registered: crate::pnworker::link::spec::NodeRegistered = response
        .json()
        .await
        .map_err(|error| format!("the coordinator sent an unreadable answer ({error})"))?;
    if registered.accepted {
        Ok(format!("registered as {node}"))
    } else {
        Err(registered.reason.unwrap_or_else(|| "refused".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // The whole point of the required/optional split: an install that has run for a year without
    // Lumiere configured must not be dragged into a wizard by an upgrade.
    #[test]
    fn only_unstartable_configuration_triggers_setup() {
        let configured = env_of(&[(TOKEN, "a-token")]);
        assert!(missing_required(Role::Coordinator, &configured).is_empty());

        let blank = env_of(&[(TOKEN, "   ")]);
        assert_eq!(missing_required(Role::Coordinator, &blank).len(), 1);
        assert!(missing_required(Role::Coordinator, &HashMap::new()).len() == 1);
    }

    #[test]
    fn a_node_needs_its_three_link_settings() {
        let missing = missing_required(Role::Node, &HashMap::new());
        let names = missing.iter().map(|k| k.key).collect::<Vec<_>>();
        assert!(names.contains(&LINK_COORDINATOR_URL), "{names:?}");
        assert!(names.contains(&LINK_NODE_NAME), "{names:?}");
        assert!(names.contains(&LINK_NODE_TOKEN), "{names:?}");
        // max_jobs has a default and must not block a start.
        assert!(!names.contains(&LINK_MAX_JOBS), "{names:?}");
    }

    // Setup may be about to write the value that decides the role, so it cannot ask a cached
    // accessor what the role is.
    #[test]
    fn the_role_comes_from_the_config_file() {
        assert_eq!(detect_role(&env_of(&[(PANDORA_MODE, "mini")])), Role::Node);
        assert_eq!(detect_role(&env_of(&[(PANDORA_MODE, "MINI")])), Role::Node);
        assert_eq!(detect_role(&env_of(&[(PANDORA_MODE, "")])), Role::Coordinator);
        assert_eq!(detect_role(&HashMap::new()), Role::Coordinator);
    }

    // A template that `get_env` cannot read would be worse than none, since the operator would
    // fill it in and get nothing.
    #[test]
    fn the_template_parses_back_as_configuration() {
        for role in [Role::Coordinator, Role::Node] {
            let body = template(role);
            let mut parsed = HashMap::new();
            for line in body.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let (key, value) = line.split_once(ENV_SEP).expect("a non-comment line must parse");
                parsed.insert(key.trim().to_string(), value.trim().to_string());
            }
            for entry in keys(role) {
                assert!(parsed.contains_key(entry.key), "{} missing from the {} template", entry.key, role.label());
            }
            if role == Role::Node {
                assert_eq!(parsed.get(PANDORA_MODE).map(String::as_str), Some("mini"));
            }
        }
    }

    #[test]
    fn obvious_typos_are_caught_without_a_network() {
        assert!(validate(API_PORT, "notaport").is_err());
        assert!(validate(API_PORT, "8787").is_ok());
        assert!(validate(LINK_MAX_JOBS, "0").is_err());
        assert!(validate(LINK_MAX_JOBS, "2").is_ok());
        // A trailing slash breaks path joining everywhere these are used.
        assert!(validate(LINK_COORDINATOR_URL, "https://example.com/").is_err());
        assert!(validate(LINK_COORDINATOR_URL, "https://example.com").is_ok());
        assert!(validate(LUMIERE_BROKER_URL, "example.com").is_err());
        assert!(validate(API_PUBLIC_URL, "ftp://example.com").is_err());
        // The node name is a field in a `|`-separated token line.
        assert!(validate(LINK_NODE_NAME, "two words").is_err());
        assert!(validate(LINK_NODE_NAME, "mini|x").is_err());
        assert!(validate(LINK_NODE_NAME, "mini-osaka").is_ok());
    }
}
