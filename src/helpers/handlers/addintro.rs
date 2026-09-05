use super::*;

use pandora_toolchain::pnworker::util::{ConcatKind, write_concat_config};

struct ConcatVariant {
    label: &'static str,
    sample_rate: &'static str,
    fps: &'static str,
}

const CONCAT_VARIANTS: &[ConcatVariant] = &[
    ConcatVariant { label: "44100_23976", sample_rate: "44100", fps: "24000/1001" },
    ConcatVariant { label: "44100_24", sample_rate: "44100", fps: "24" },
    ConcatVariant { label: "48000_23976", sample_rate: "48000", fps: "24000/1001" },
    ConcatVariant { label: "48000_24", sample_rate: "48000", fps: "24" },
];

// `/touchintro` and `/touchoutro` are the same command with a different registry to write into and
// a different folder to install under. Sharing the body is what keeps the two from drifting: the
// variant grid an outro is encoded to has to be the one an intro is encoded to, or a preset that
// concats both would need two different compatibility passes.
pub async fn handle_addintro(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    handle_addconcat(ctx, command, ConcatKind::Intro).await;
}

pub async fn handle_addoutro(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    handle_addconcat(ctx, command, ConcatKind::Outro).await;
}

async fn handle_addconcat(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    kind: ConcatKind,
) {
    let label = kind.label();
    let server_id = match command_server_id(ctx, command, &format!("/touch{}", label)).await {
        Some(id) => id,
        None => return,
    };
    let name = match option_trimmed(command, "name") {
        Some(s) if valid_concat_name(&s) => s,
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

    // Intro and outro groups share a name space per server only within their own kind, so they are
    // installed under separate roots: two groups both called `summer` must not overwrite each other.
    let out_dir = PathBuf::from("DB")
        .join(concat_root(kind))
        .join(server_id.to_string());
    let final_dir = out_dir.join(&name);
    let tmp_dir = PathBuf::from("DB")
        .join("work")
        .join(format!("add{}_{}_{}", label, server_id, command.id.get()));
    let encoded_dir = tmp_dir.join("encoded");
    if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
        addintro_response(ctx, command, format!("Failed to create concat dir: {}", e)).await;
        return;
    }
    if let Err(e) = tokio::fs::create_dir_all(&encoded_dir).await {
        addintro_response(ctx, command, format!("Failed to create temp dir: {}", e)).await;
        return;
    }

    let input = tmp_dir.join("input");
    if let Err(e) = tokio::fs::write(&input, &bytes).await {
        addintro_response(ctx, command, format!("Failed to write uploaded video: {}", e)).await;
        cleanup_addintro_tmp(&tmp_dir).await;
        return;
    }

    let mut file_names = Vec::new();
    for (idx, variant) in CONCAT_VARIANTS.iter().enumerate() {
        addintro_response(ctx, command, format!("Encoding variant {}/{} (`{}`)...", idx + 1, CONCAT_VARIANTS.len(), variant.label)).await;
        let file_name = format!("{}_{}.mp4", name, variant.label);
        let tmp_output = encoded_dir.join(&file_name);
        match encode_concat_variant(&input, &tmp_output, variant).await {
            Ok(()) => {}
            Err(e) => {
                addintro_response(ctx, command, format!("Failed to encode `{}`: {}", variant.label, e)).await;
                cleanup_addintro_tmp(&tmp_dir).await;
                return;
            }
        }
        file_names.push(file_name);
    }

    let previous_dir = out_dir.join(format!(".{}_previous_{}", name, command.id.get()));
    tokio::fs::remove_dir_all(&previous_dir).await.ok();
    let had_previous = final_dir.exists();
    if had_previous {
        if let Err(e) = tokio::fs::rename(&final_dir, &previous_dir).await {
            addintro_response(ctx, command, format!("Failed to stage replacement for `{}`: {}", final_dir.display(), e)).await;
            cleanup_addintro_tmp(&tmp_dir).await;
            return;
        }
    }
    if let Err(e) = tokio::fs::rename(&encoded_dir, &final_dir).await {
        if had_previous {
            tokio::fs::rename(&previous_dir, &final_dir).await.ok();
        }
        addintro_response(ctx, command, format!("Failed to install `{}`: {}", final_dir.display(), e)).await;
        cleanup_addintro_tmp(&tmp_dir).await;
        return;
    }
    if had_previous {
        tokio::fs::remove_dir_all(&previous_dir).await.ok();
    }

    match upsert_concat_group(kind, &name, final_dir.display().to_string()).await {
        Ok(()) => {
            cleanup_addintro_tmp(&tmp_dir).await;
            let paths = file_names.iter().map(|file| final_dir.join(file).display().to_string()).collect::<Vec<_>>();
            let content = format!("Added {} group `{}` with {} variants in `{}`:\n{}", label, name, paths.len(), final_dir.display(), paths.iter().map(|p| format!("`{}`", p)).collect::<Vec<_>>().join("\n"));
            command
                .edit_response(
                    ctx,
                    EditInteractionResponse::new()
                        .content("")
                        .embed(success_embed(command, COMMAND_UPDATED).description(content)),
                )
                .await
                .ok();
        }
        Err(e) => {
            cleanup_addintro_tmp(&tmp_dir).await;
            let file = Path::new(kind.config_path())
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| kind.config_path().to_string());
            addintro_response(ctx, command, format!("Encoded files, but failed to update {}: {}", file, e)).await;
        }
    }
}

// `DB/concat` is where intro groups have always been installed; renaming it would strand every
// group an operator already registered, so only the outro root is new.
fn concat_root(kind: ConcatKind) -> &'static str {
    match kind {
        ConcatKind::Intro => "concat",
        ConcatKind::Outro => "concat-outro",
    }
}

fn valid_concat_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

async fn encode_concat_variant(input: &Path, output: &Path, variant: &ConcatVariant) -> Result<(), String> {
    let input = input.display().to_string();
    let output = output.display().to_string();
    let fps = variant.fps.to_string();
    let sample_rate = variant.sample_rate.to_string();
    let ok = tokio::task::spawn_blocking(move || {
        use pandora_toolchain::lib::mpeg::core::{FfmpegParams, run_ffmpeg_params};
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

async fn upsert_concat_group(kind: ConcatKind, name: &str, folder: String) -> Result<(), String> {
    let name = name.to_string();
    tokio::task::spawn_blocking(move || {
        let mut config = ConcatConfig::load_kind(kind);
        config.groups.insert(name, folder);
        write_concat_config(kind, &config)
    })
    .await
    .map_err(|e| e.to_string())?
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
