use super::*;
use pandora_toolchain::lib::env::standard::{
    AKIRA_API, AKIRA_TOKEN, ANIZM_EMAIL, ANIZM_PASSWORD, OPENANIME_EMAIL, OPENANIME_PASSWORD,
};
use pandora_toolchain::pnworker::server_config::{drive_only_from_meta, hls_from_meta};

pub async fn handle_providers(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let env = get_pandora_env();
    let server_meta = command.guild_id
        .and_then(|g| std::fs::read_to_string(format!("DB/config/{}/meta.pandora", g.get())).ok());
    let server_lines: Vec<&str> = server_meta
        .as_deref()
        .map(|s| s.lines().collect())
        .unwrap_or_default();
    let drive_only = server_meta
        .as_deref()
        .map(drive_only_from_meta)
        .unwrap_or(false);
    let hls = server_meta
        .as_deref()
        .map(hls_from_meta)
        .unwrap_or(false);

    let local_gdrive_enabled = !matches!(
        server_lines.get(9).copied().unwrap_or("true").trim(),
        "false" | "0" | "disabled" | "off"
    );
    let requested_profile = command
        .guild_id
        .filter(|_| local_gdrive_enabled)
        .map(|guild| guild_drive_profile(guild.get()));
    let broker_status = match LumiereClient::from_env() {
        Ok(client) => client
            .provider_status(requested_profile.as_deref())
            .await
            .unwrap_or_default(),
        Err(_) => Default::default(),
    };
    let active_server_gdrive = local_gdrive_enabled && broker_status.requested_drive;
    let global_gdrive = broker_status.global_drive;
    let gdrive_label = if active_server_gdrive {
        "server via Lumiere"
    } else if global_gdrive && !local_gdrive_enabled {
        "global via Lumiere (server disabled)"
    } else if global_gdrive {
        "global via Lumiere"
    } else {
        "not attached"
    };

    let persistence = server_lines.get(1).copied().unwrap_or("").trim();
    let github_attached = persistence.starts_with("https://github.com/") || persistence.starts_with("http://github.com/");
    let forgejo_attached = !persistence.is_empty() && !github_attached;

    let upload_lines = vec![
        if hls {
            "🔒 Server policy: Lumiere HLS only".to_string()
        } else if drive_only {
            "🔒 Server policy: Google Drive only".to_string()
        } else {
            "Server policy: all configured providers".to_string()
        },
        if hls {
            "— Google Drive (disabled by HLS-only policy)".to_string()
        } else {
            attached_line_with_note("Google Drive", active_server_gdrive || global_gdrive, gdrive_label)
        },
        policy_provider_line("Byse (via Lumiere)", broker_status.byse, drive_only || hls),
        policy_provider_line("LuluStream (via Lumiere)", broker_status.lulustream, drive_only || hls),
        policy_provider_line("Voe (via Lumiere)", broker_status.voe, drive_only || hls),
        attached_line("Lumiere Files HLS-only output (12 hours)", hls),
    ].join("\n");

    let distribution_lines = vec![
        attached_line(
            "OpenAnime (via Capella)",
            env_set(&env, OPENANIME_EMAIL) && env_set(&env, OPENANIME_PASSWORD),
        ),
        attached_line(
            "Anizm (via Capella)",
            env_set(&env, ANIZM_EMAIL) && env_set(&env, ANIZM_PASSWORD),
        ),
        attached_line(
            "Akira (via Capella)",
            env_set(&env, AKIRA_API) && env_set(&env, AKIRA_TOKEN),
        ),
        attached_line("AnimeciX (via Capella)", env_set(&env, ANIMECIX)),
        attached_line("AniSub (via Capella)", env_set(&env, ANISUB)),
    ].join("\n");

    let persistence_lines = vec![
        attached_line("GitHub organisations", github_attached),
        attached_line("ForgeJo organisations", forgejo_attached),
    ].join("\n");

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().embed(
            CreateEmbed::new()
                .title("Pandora providers")
                .description("Currently attached APIs and built-in providers for this server.")
                .field("Download", [
                    active_line("Nyaa links"),
                    active_line("Any .torrent link"),
                    active_line("Any magnet"),
                    active_line("Google Drive links"),
                    active_line("Direct video file links"),
                ].join("\n"), false)
                .field("Encode", active_line("CPU encode provided by Pandora"), false)
                .field("Upload", upload_lines, false)
                .field("Distribution", distribution_lines, false)
                .field("Persistence", persistence_lines, false)
        )
    )).await.ok();
}

fn env_set(env: &HashMap<String, String>, key: &str) -> bool {
    env.get(key).map(|s| !s.trim().is_empty()).unwrap_or(false)
}

fn active_line(name: &str) -> String {
    format!("✅ {}", name)
}

fn attached_line(name: &str, active: bool) -> String {
    if active {
        format!("✅ {}", name)
    } else {
        format!("— {}", name)
    }
}

fn policy_provider_line(name: &str, configured: bool, drive_only: bool) -> String {
    if drive_only {
        format!("— {} (disabled by server policy)", name)
    } else {
        attached_line(name, configured)
    }
}

fn attached_line_with_note(name: &str, active: bool, note: &str) -> String {
    if active {
        format!("✅ {} ({})", name, note)
    } else {
        format!("— {}", name)
    }
}

#[cfg(test)]
mod tests {
    use super::policy_provider_line;

    #[test]
    fn drive_only_policy_overrides_configured_provider_status() {
        assert_eq!(
            policy_provider_line("Byse", true, true),
            "— Byse (disabled by server policy)",
        );
        assert_eq!(policy_provider_line("Byse", true, false), "✅ Byse");
    }
}
