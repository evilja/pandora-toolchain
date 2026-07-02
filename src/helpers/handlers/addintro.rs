use super::*;

const INTROS_PATH: &str = "DB/config/global/environment/intros.toml";

struct IntroVariant {
    label: &'static str,
    sample_rate: &'static str,
    fps: &'static str,
}

const INTRO_VARIANTS: &[IntroVariant] = &[
    IntroVariant { label: "44100_23976", sample_rate: "44100", fps: "24000/1001" },
    IntroVariant { label: "44100_24", sample_rate: "44100", fps: "24" },
    IntroVariant { label: "48000_23976", sample_rate: "48000", fps: "24000/1001" },
    IntroVariant { label: "48000_24", sample_rate: "48000", fps: "24" },
];

pub async fn handle_addintro(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/addintro").await {
        Some(id) => id,
        None => return,
    };
    let name = match option_trimmed(command, "name") {
        Some(s) if valid_intro_name(&s) => s,
        Some(_) => {
            command_error(ctx, command, "Error: `name` may only contain letters, numbers, `_`, and `-`.").await;
            return;
        }
        None => {
            command_error(ctx, command, "Error: `name` is required.").await;
            return;
        }
    };
    let attachment = match option_attachment(command, "video") {
        Some(a) => a,
        None => {
            command_error(ctx, command, "Error: `video` attachment is required.").await;
            return;
        }
    };

    if command.create_response(ctx, CreateInteractionResponse::Defer(
        CreateInteractionResponseMessage::new().ephemeral(true)
    )).await.is_err() {
        return;
    }

    addintro_response(ctx, command, "Downloading attachment...").await;
    let bytes = match attachment.download().await {
        Ok(b) => b,
        Err(e) => {
            addintro_response(ctx, command, format!("Failed to download attachment: {}", e)).await;
            return;
        }
    };

    let out_dir = PathBuf::from("DB").join("concat").join(server_id.to_string());
    let tmp_dir = PathBuf::from("DB")
        .join("work")
        .join(format!("addintro_{}_{}", server_id, command.id.get()));
    if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
        addintro_response(ctx, command, format!("Failed to create concat dir: {}", e)).await;
        return;
    }
    if let Err(e) = tokio::fs::create_dir_all(&tmp_dir).await {
        addintro_response(ctx, command, format!("Failed to create temp dir: {}", e)).await;
        return;
    }

    let input = tmp_dir.join("input");
    if let Err(e) = tokio::fs::write(&input, &bytes).await {
        addintro_response(ctx, command, format!("Failed to write uploaded video: {}", e)).await;
        cleanup_addintro_tmp(&tmp_dir).await;
        return;
    }

    let mut paths = Vec::new();
    for (idx, variant) in INTRO_VARIANTS.iter().enumerate() {
        addintro_response(ctx, command, format!("Encoding variant {}/4 (`{}`)...", idx + 1, variant.label)).await;
        let tmp_output = tmp_dir.join(format!("{}_{}.mp4", name, variant.label));
        let final_output = out_dir.join(format!("{}_{}.mp4", name, variant.label));
        match encode_intro_variant(&input, &tmp_output, variant).await {
            Ok(()) => {}
            Err(e) => {
                addintro_response(ctx, command, format!("Failed to encode `{}`: {}", variant.label, e)).await;
                cleanup_addintro_tmp(&tmp_dir).await;
                return;
            }
        }
        if let Err(e) = tokio::fs::rename(&tmp_output, &final_output).await {
            addintro_response(ctx, command, format!("Failed to move `{}`: {}", final_output.display(), e)).await;
            cleanup_addintro_tmp(&tmp_dir).await;
            return;
        }
        paths.push(final_output.display().to_string());
    }

    match upsert_intro_group(&name, paths.clone()).await {
        Ok(()) => {
            cleanup_addintro_tmp(&tmp_dir).await;
            addintro_response(ctx, command, format!("Added intro group `{}` with {} variants:\n{}", name, paths.len(), paths.iter().map(|p| format!("`{}`", p)).collect::<Vec<_>>().join("\n"))).await;
        }
        Err(e) => {
            cleanup_addintro_tmp(&tmp_dir).await;
            addintro_response(ctx, command, format!("Encoded files, but failed to update intros.toml: {}", e)).await;
        }
    }
}

fn valid_intro_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

async fn encode_intro_variant(input: &Path, output: &Path, variant: &IntroVariant) -> Result<(), String> {
    let input = input.display().to_string();
    let output = output.display().to_string();
    let fps = variant.fps.to_string();
    let sample_rate = variant.sample_rate.to_string();
    let ok = tokio::task::spawn_blocking(move || {
        use pandora_toolchain::libpnmpeg::core::{FfmpegParams, run_ffmpeg_params};
        use std::borrow::Cow;

        run_ffmpeg_params(vec![
            FfmpegParams::Overwrite,
            FfmpegParams::Input(Cow::Owned(input)),
            FfmpegParams::Map(Cow::Borrowed("0:v:0")),
            FfmpegParams::Map(Cow::Borrowed("0:a?")),
            FfmpegParams::Cv(Cow::Borrowed("libx264")),
            FfmpegParams::BasicFilter(Cow::Borrowed("format=yuv420p")),
            FfmpegParams::R(Cow::Owned(fps)),
            FfmpegParams::Ca(Cow::Borrowed("aac")),
            FfmpegParams::Ar(Cow::Owned(sample_rate)),
            FfmpegParams::Movflags,
            FfmpegParams::Output(Cow::Owned(output)),
        ])
    }).await.map_err(|e| e.to_string())?;
    if ok {
        Ok(())
    } else {
        Err("ffmpeg failed".to_string())
    }
}

async fn upsert_intro_group(name: &str, paths: Vec<String>) -> Result<(), String> {
    if let Some(parent) = Path::new(INTROS_PATH).parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
    }
    let contents = tokio::fs::read_to_string(INTROS_PATH).await.unwrap_or_default();
    let mut config: IntrosConfig = if contents.trim().is_empty() {
        IntrosConfig { groups: HashMap::new() }
    } else {
        toml::from_str(&contents).map_err(|e| e.to_string())?
    };
    config.groups.insert(name.to_string(), paths);
    let body = toml::to_string_pretty(&config).map_err(|e| e.to_string())?;
    tokio::fs::write(INTROS_PATH, body).await.map_err(|e| e.to_string())
}

async fn addintro_response(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    content: impl Into<String>,
) {
    command.edit_response(ctx, EditInteractionResponse::new().content(content.into())).await.ok();
}

async fn cleanup_addintro_tmp(tmp_dir: &Path) {
    tokio::fs::remove_dir_all(tmp_dir).await.ok();
}
