use super::*;
use pandora_toolchain::lib::env::standard::{
    AKIRA_API, AKIRA_TOKEN, ANIZM_EMAIL, ANIZM_PASSWORD, OPENANIME_EMAIL, OPENANIME_PASSWORD,
};

pub async fn handle_providers(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let env = get_pandora_env();
    let server_meta = command.guild_id
        .and_then(|g| std::fs::read_to_string(format!("DB/config/{}/meta.pandora", g.get())).ok());
    let server_lines: Vec<&str> = server_meta
        .as_deref()
        .map(|s| s.lines().collect())
        .unwrap_or_default();

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
        attached_line_with_note("Google Drive", active_server_gdrive || global_gdrive, gdrive_label),
        attached_line("Doodstream (via Lumiere)", broker_status.doodstream),
        attached_line("LuluStream (via Lumiere)", broker_status.lulustream),
        attached_line("Voe (via Lumiere)", broker_status.voe),
        attached_line_with_note("Abyss", broker_status.abyss, "remote API unsupported"),
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

fn attached_line_with_note(name: &str, active: bool, note: &str) -> String {
    if active {
        format!("✅ {} ({})", name, note)
    } else {
        format!("— {}", name)
    }
}
