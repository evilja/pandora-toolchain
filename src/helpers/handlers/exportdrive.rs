use super::*;
use pandora_toolchain::lib::env::core::get_pandora_env;
use pandora_toolchain::lib::env::standard::{
    CLIENT_ID, CLIENT_SECRET, PARENTID, REFRESH_TOKEN, TOKEN_URL,
};
use serde::Serialize;
use serenity::builder::CreateAttachment;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;

const DEFAULT_GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const MAX_WORKER_SECRET_BYTES: usize = 5 * 1024;

#[derive(Debug, Serialize)]
struct ExportDriveProfile {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    token_url: String,
    roots: BTreeMap<String, String>,
}

pub async fn handle_exportdrive(ctx: &Context, command: &serenity::all::CommandInteraction) {
    if !has_level_at_least(command.user.id.get(), 4) {
        command_error(ctx, command, "Error: `/exportdrive` requires Witch rank.").await;
        return;
    }
    let recipient = match option_trimmed(command, "recipient") {
        Some(recipient) => recipient,
        None => {
            command_error(ctx, command, "Error: an age X25519 recipient is required.").await;
            return;
        }
    };

    let export = match collect_drive_profiles().await {
        Ok(export) => export,
        Err(error) => {
            command_error(ctx, command, format!("Drive export failed: {error}")).await;
            return;
        }
    };
    let profile_count = export.profiles.len();
    let skipped_count = export.skipped_guilds.len();
    let mut plaintext = match serde_json::to_vec(&export.profiles) {
        Ok(plaintext) => plaintext,
        Err(_) => {
            command_error(ctx, command, "Drive export failed while encoding JSON.").await;
            return;
        }
    };
    drop(export);
    let plaintext_len = plaintext.len();
    if plaintext_len > MAX_WORKER_SECRET_BYTES {
        plaintext.fill(0);
        command_error(
            ctx,
            command,
            format!(
                "Drive export is {plaintext_len} bytes, exceeding Cloudflare's {MAX_WORKER_SECRET_BYTES}-byte secret limit. Split the profiles before migration."
            ),
        )
        .await;
        return;
    }
    let encrypted = match encrypt_for_age_recipient(&recipient, &mut plaintext) {
        Ok(encrypted) => encrypted,
        Err(error) => {
            command_error(ctx, command, error).await;
            return;
        }
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let filename = format!("lumiere-drive-profiles-{timestamp}.json.age");
    let attachment = CreateAttachment::bytes(encrypted, filename.clone());
    let skipped = if skipped_count == 0 {
        String::new()
    } else {
        format!(
            " {skipped_count} incomplete guild configuration(s) were skipped because they did not contain complete OAuth credentials and a Drive root."
        )
    };
    println!(
        "[exportdrive] encrypted profile export user={} profiles={} skipped={} json_bytes={}",
        command.user.id.get(),
        profile_count,
        skipped_count,
        plaintext_len,
    );
    command
        .create_response(
            ctx,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!(
                        "Encrypted {profile_count} Worker Drive profile(s) to the supplied age recipient.{skipped}\nDecrypt `{filename}` only on the trusted machine holding the matching identity, validate it with `jq`, then install it with `wrangler secret put LUMIERE_DRIVE_PROFILES`."
                    ))
                    .add_file(attachment)
                    .ephemeral(true),
            ),
        )
        .await
        .ok();
}

struct DriveProfileExport {
    profiles: BTreeMap<String, ExportDriveProfile>,
    skipped_guilds: Vec<u64>,
}

async fn collect_drive_profiles() -> Result<DriveProfileExport, String> {
    let env = get_pandora_env();
    let token_url =
        env_value(&env, TOKEN_URL).unwrap_or_else(|| DEFAULT_GOOGLE_TOKEN_URL.to_string());
    let mut profiles = BTreeMap::new();
    let global_parent = required_env_value(&env, PARENTID)?;
    if !valid_drive_id(&global_parent) {
        return Err("global Drive parent id is invalid".to_string());
    }
    let mut global_roots = BTreeMap::new();
    global_roots.insert("default".to_string(), global_parent);
    profiles.insert(
        "global".to_string(),
        ExportDriveProfile {
            client_id: required_env_value(&env, CLIENT_ID)?,
            client_secret: required_env_value(&env, CLIENT_SECRET)?,
            refresh_token: required_env_value(&env, REFRESH_TOKEN)?,
            token_url: token_url.clone(),
            roots: global_roots,
        },
    );
    drop(env);

    let config_root = Path::new("DB").join("config");
    let mut entries = tokio::fs::read_dir(&config_root)
        .await
        .map_err(|_| "Pandora guild configuration directory is unavailable".to_string())?;
    let mut skipped_guilds = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|_| "failed to enumerate Pandora guild configurations".to_string())?
    {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(guild_id) = name.parse::<u64>() else {
            continue;
        };
        let raw = match tokio::fs::read_to_string(entry.path().join("meta.pandora")).await {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                skipped_guilds.push(guild_id);
                continue;
            }
        };
        match profile_from_guild_meta(&raw, &token_url) {
            Some(profile) => {
                profiles.insert(format!("guild:{guild_id}"), profile);
            }
            None if guild_meta_has_drive_data(&raw) => skipped_guilds.push(guild_id),
            None => {}
        }
    }
    skipped_guilds.sort_unstable();
    Ok(DriveProfileExport {
        profiles,
        skipped_guilds,
    })
}

fn profile_from_guild_meta(raw: &str, token_url: &str) -> Option<ExportDriveProfile> {
    let lines = raw.lines().collect::<Vec<_>>();
    let client_id = meta_value(&lines, 4)?;
    let client_secret = meta_value(&lines, 5)?;
    let refresh_token = meta_value(&lines, 6)?;
    let mut roots = BTreeMap::new();
    if let Some(root) = meta_value(&lines, 7).filter(|root| valid_drive_id(root)) {
        roots.insert("smartcode".to_string(), root.to_string());
    }
    if let Some(root) = meta_value(&lines, 10).filter(|root| valid_drive_id(root)) {
        roots.insert("anonymous".to_string(), root.to_string());
    }
    if roots.is_empty() {
        return None;
    }
    Some(ExportDriveProfile {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        refresh_token: refresh_token.to_string(),
        token_url: token_url.to_string(),
        roots,
    })
}

fn guild_meta_has_drive_data(raw: &str) -> bool {
    let lines = raw.lines().collect::<Vec<_>>();
    [4usize, 5, 6, 7, 10]
        .into_iter()
        .any(|index| meta_value(&lines, index).is_some())
}

fn meta_value<'a>(lines: &'a [&str], index: usize) -> Option<&'a str> {
    lines
        .get(index)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
}

fn env_value(env: &HashMap<String, String>, key: &str) -> Option<String> {
    env.get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn required_env_value(env: &HashMap<String, String>, key: &str) -> Result<String, String> {
    env_value(env, key).ok_or_else(|| format!("global `{key}` is not configured"))
}

fn valid_drive_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn encrypt_for_age_recipient(recipient: &str, plaintext: &mut [u8]) -> Result<Vec<u8>, String> {
    let result = (|| {
        let recipient = recipient.parse::<age::x25519::Recipient>().map_err(|_| {
            "Error: recipient must be a valid age X25519 public key (`age1...`).".to_string()
        })?;
        let encryptor =
            age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
                .map_err(|_| "Drive export encryption initialization failed.".to_string())?;
        let mut encrypted = Vec::new();
        let mut writer = encryptor
            .wrap_output(&mut encrypted)
            .map_err(|_| "Drive export encryption failed.".to_string())?;
        writer
            .write_all(plaintext)
            .map_err(|_| "Drive export encryption failed.".to_string())?;
        writer
            .finish()
            .map_err(|_| "Drive export encryption failed.".to_string())?;
        Ok(encrypted)
    })();
    plaintext.fill(0);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn guild_profile_uses_only_complete_credentials_and_valid_roots() {
        let raw = "en\nforgejo\nchannel\napi\nclient\nsecret\nrefresh\nsmart_root\nwrap\ntrue\nanon_root\n";
        let profile = profile_from_guild_meta(raw, DEFAULT_GOOGLE_TOKEN_URL).unwrap();
        assert_eq!(profile.client_id, "client");
        assert_eq!(profile.roots["smartcode"], "smart_root");
        assert_eq!(profile.roots["anonymous"], "anon_root");

        let incomplete = "en\nforgejo\nchannel\napi\nclient\n\nrefresh\nsmart_root\n";
        assert!(profile_from_guild_meta(incomplete, DEFAULT_GOOGLE_TOKEN_URL).is_none());
        assert!(guild_meta_has_drive_data(incomplete));
    }

    #[test]
    fn age_export_round_trips_and_clears_plaintext() {
        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public().to_string();
        let mut plaintext = br#"{"global":{"client_secret":"secret"}}"#.to_vec();
        let encrypted = encrypt_for_age_recipient(&recipient, &mut plaintext).unwrap();
        assert!(plaintext.iter().all(|byte| *byte == 0));

        let decryptor = age::Decryptor::new(&encrypted[..]).unwrap();
        let mut reader = decryptor
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .unwrap();
        let mut decrypted = Vec::new();
        reader.read_to_end(&mut decrypted).unwrap();
        assert_eq!(decrypted, br#"{"global":{"client_secret":"secret"}}"#);
    }
}
