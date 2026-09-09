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

    match install_concat_upload(server_id, command.id.get(), kind, &name, &bytes, |index, variant| {
        addintro_response(ctx, command, format!("Encoding variant {}/{} (`{}`)...", index, CONCAT_VARIANTS.len(), variant))
    }).await {
        Ok(paths) => {
            command.edit_response(ctx, EditInteractionResponse::new().content("")
                .embed(success_embed(command, COMMAND_UPDATED).description(format!(
                    "Added {} group `{}` with {} variants.", label, name, paths.len()
                )))).await.ok();
        }
        Err(error) => addintro_response(ctx, command, error).await,
    }
}

// Shared by slash uploads and the setup wizard, so both install the same compatibility variants.
pub(super) async fn install_concat_upload<F, Fut>(
    server_id: u64, operation: u64, kind: ConcatKind, name: &str, bytes: &[u8], mut progress: F,
) -> Result<Vec<PathBuf>, String>
where F: FnMut(usize, &'static str) -> Fut, Fut: std::future::Future<Output = ()>
{
    if !valid_concat_name(name) { return Err("Invalid group name".into()); }
    let out_dir = PathBuf::from("DB").join(concat_root(kind)).join(server_id.to_string());
    let final_dir = out_dir.join(name);
    let tmp_dir = PathBuf::from("DB").join("work").join(format!("add{}_{}_{}", kind.label(), server_id, operation));
    let encoded_dir = tmp_dir.join("encoded");
    let result = async {
        tokio::fs::create_dir_all(&out_dir).await.map_err(|e| e.to_string())?;
        tokio::fs::create_dir_all(&encoded_dir).await.map_err(|e| e.to_string())?;
        let input = tmp_dir.join("input");
        tokio::fs::write(&input, bytes).await.map_err(|e| e.to_string())?;
        let mut paths = Vec::new();
        for (index, variant) in CONCAT_VARIANTS.iter().enumerate() {
            progress(index + 1, variant.label).await;
            let file_name = format!("{}_{}.mp4", name, variant.label);
            encode_concat_variant(&input, &encoded_dir.join(&file_name), variant).await?;
            paths.push(final_dir.join(file_name));
        }
        let previous = out_dir.join(format!(".{}_previous_{}", name, operation));
        let had_previous = final_dir.exists();
        if had_previous {
            tokio::fs::rename(&final_dir, &previous).await.map_err(|e| e.to_string())?;
        }
        if let Err(error) = tokio::fs::rename(&encoded_dir, &final_dir).await {
            if had_previous { tokio::fs::rename(&previous, &final_dir).await.ok(); }
            return Err(error.to_string());
        }
        if let Err(error) = upsert_concat_group(kind, name, final_dir.display().to_string()).await {
            tokio::fs::remove_dir_all(&final_dir).await.ok();
            if had_previous { tokio::fs::rename(&previous, &final_dir).await.ok(); }
            return Err(error);
        }
        if had_previous { tokio::fs::remove_dir_all(&previous).await.ok(); }
        Ok(paths)
    }.await;
    cleanup_addintro_tmp(&tmp_dir).await;
    result
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
