use super::*;

pub async fn handle_providers(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let env = get_pandora_env();
    let server_meta = command.guild_id
        .and_then(|g| std::fs::read_to_string(format!("DB/config/{}/meta.pandora", g.get())).ok());
    let server_lines: Vec<&str> = server_meta
        .as_deref()
        .map(|s| s.lines().collect())
        .unwrap_or_default();

    let global_gdrive = env_set(&env, CLIENT_ID)
        && env_set(&env, CLIENT_SECRET)
        && env_set(&env, REFRESH_TOKEN)
        && env_set(&env, PARENTID);
    let server_gdrive = [4usize, 5, 6, 7].iter()
        .all(|idx| server_lines.get(*idx).map(|s| !s.trim().is_empty()).unwrap_or(false));
    let gdrive_label = if server_gdrive {
        "server"
    } else if global_gdrive {
        "global"
    } else {
        "not attached"
    };

    let persistence = server_lines.get(1).copied().unwrap_or("").trim();
    let github_attached = persistence.starts_with("https://github.com/") || persistence.starts_with("http://github.com/");
    let forgejo_attached = !persistence.is_empty() && !github_attached;

    let upload_lines = vec![
        attached_line_with_note("Google Drive", server_gdrive || global_gdrive, gdrive_label),
        attached_line("Doodstream", env_set(&env, DOODSTREAM)),
        attached_line("LuluStream", env_set(&env, LULU)),
        attached_line("Voe", env_set(&env, VOESX)),
        attached_line("Abyss", env_set(&env, ABYSS)),
    ].join("\n");

    let distribution_lines = vec![
        attached_line("AnimeciX", env_set(&env, ANIMECIX)),
        attached_line("AniSub", env_set(&env, ANISUB)),
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
