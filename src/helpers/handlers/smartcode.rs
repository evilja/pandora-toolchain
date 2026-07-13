use super::*;
use pandora_toolchain::pnworker::core::PreviewRequest;
use pandora_toolchain::pnworker::preview::{
    DEFAULT_COOLDOWN_CS, select_shots_with_stamps_and_cooldown,
};

const MAX_PREVIEW_COOLDOWN_SECONDS: i64 = 3600;

pub async fn handle_smartcode(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) -> Option<Job> {
    let mut response_msg = working_response(ctx, command, "Working…").await?;
    let result = smartcode_merge_upload(ctx, command, &mut response_msg, "/smartcode", "smartcode").await?;

    let _ = response_msg.edit(ctx, EditMessage::new().content("...")).await;

    response_msg.react(ctx, '❌').await.ok();

    let final_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    let mut job = Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        final_msg.id.get(),
        JobType::Encode,
        final_msg.id.get(),
        nyaaise(&result.link),
        result.merged_bytes,
        ctx.clone(),
        final_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    );
    job.acix = build_acix_publish(ctx, command).await;
    job.smartcode_drive_name = Some(
        pandora_toolchain::pnworker::core::SmartcodeDriveName::new(
            &result.owner_repo,
            &result.gdrive_folder_local,
            result.episode,
        ),
    );
    job.gdrive_folder_global = Some(result.gdrive_folder_global);
    job.gdrive_folder_local = Some(result.gdrive_folder_local);
    Some(job)
}

pub async fn handle_smartcode_preview(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) -> Option<Job> {
    let mut response_msg = working_response(ctx, command, "Working…").await?;
    let result =
        smartcode_merge_upload(
            ctx,
            command,
            &mut response_msg,
            "/smartcode preview",
            "smartcode-preview",
        )
            .await?;

    let tl = match load_preview_ass(response_msg.id.get(), "tl", &result.tl_bytes).await {
        Ok(script) => script,
        Err(e) => {
            let _ = response_msg
                .edit(
                    ctx,
                    EditMessage::new().content(format!("Failed to prepare TL preview file: {}", e)),
                )
                .await;
            return None;
        }
    };
    let ts = match result.ts_bytes.as_deref() {
        Some(bytes) => match load_preview_ass(response_msg.id.get(), "ts", bytes).await {
            Ok(script) => Some(script),
            Err(e) => {
                let _ = response_msg
                    .edit(
                        ctx,
                        EditMessage::new().content(format!("Failed to prepare TS preview file: {}", e)),
                    )
                    .await;
                return None;
            }
        },
        None => None,
    };
    let mut scripts = vec![&tl];
    if let Some(ts) = ts.as_ref() {
        scripts.push(ts);
    }
    let cooldown_seconds = match option_i64(command, "cooldown") {
        Some(seconds) if (0..=MAX_PREVIEW_COOLDOWN_SECONDS).contains(&seconds) => seconds as u64,
        Some(_) => {
            let _ = response_msg
                .edit(
                    ctx,
                    EditMessage::new().content(format!(
                        "Preview cooldown must be between 0 and {} seconds.",
                        MAX_PREVIEW_COOLDOWN_SECONDS
                    )),
                )
                .await;
            return None;
        }
        None => DEFAULT_COOLDOWN_CS / 100,
    };
    let selection = select_shots_with_stamps_and_cooldown(
        &scripts,
        ts.as_ref(),
        3,
        1000,
        cooldown_seconds * 100,
    );
    if selection.shots.is_empty() {
        let _ = response_msg
            .edit(
                ctx,
                EditMessage::new()
                    .content("Episode has no stamp marks and no typeset lines to preview."),
            )
            .await;
        return None;
    }

    let watermark_font = match command.guild_id {
        Some(guild_id) => resolve_preview_watermark_font_path(guild_id.get()).await,
        None => None,
    };

    let _ = response_msg
        .edit(ctx, EditMessage::new().content("..."))
        .await;
    response_msg.react(ctx, '❌').await.ok();
    let final_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    let mut job = Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        final_msg.id.get(),
        JobType::Preview,
        final_msg.id.get(),
        nyaaise(&result.link),
        result.merged_bytes,
        ctx.clone(),
        final_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    );
    job.preview = Some(PreviewRequest {
        shots: selection
            .shots
            .into_iter()
            .map(|shot| (shot.centiseconds, shot.label))
            .collect(),
        watermark_font,
        ranking_log: selection.ranking_log,
    });
    Some(job)
}

async fn load_preview_ass(job_id: u64, kind: &str, bytes: &[u8]) -> Result<SubstationAlpha, String> {
    let path = preview_temp_ass_path(job_id, kind);
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| e.to_string())?;
    let script = SubstationAlpha::load(path.clone(), true).await;
    let _ = tokio::fs::remove_file(path).await;
    Ok(script)
}

fn preview_temp_ass_path(job_id: u64, kind: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("pandora_preview_{}_{}_{}.ass", kind, job_id, nanos))
}

async fn build_acix_publish(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) -> Option<pandora_toolchain::pnworker::core::AcixPublish> {
    let server_id = command.guild_id?.get();
    let channel_id = command.channel_id.get();
    let meta = read_channel_meta(server_id, channel_id);
    let template = meta.acix_template?;
    let name = meta.name.clone()?;
    let mal_id = meta.mal_id? as i64;
    let episode = positive_u32_option(ctx, command, "episode").await? as i64;
    let (season_num, episode_num) = if meta.kind.as_deref() == Some("Movie") {
        (None, None)
    } else {
        (Some(meta.season as i64), Some(episode))
    };
    Some(pandora_toolchain::pnworker::core::AcixPublish {
        name,
        mal_id,
        season_num,
        episode_num,
        template,
        extra: build_extra(&meta),
    })
}

fn build_extra(meta: &ChannelMeta) -> String {
    let mut parts = Vec::new();
    if meta.tl != "---" { parts.push(format!("Çeviri: {}", meta.tl)); }
    if meta.tlc != "---" { parts.push(format!("Redaktör: {}", meta.tlc)); }
    if meta.ts != "---" { parts.push(format!("Tipset: {}", meta.ts)); }
    if meta.qc != "---" { parts.push(format!("Kalite Kontrol: {}", meta.qc)); }
    parts.join(" ")
}
