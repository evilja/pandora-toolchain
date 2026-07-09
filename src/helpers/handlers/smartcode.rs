use super::*;
use pandora_toolchain::pnworker::core::PreviewRequest;
use pandora_toolchain::pnworker::preview::select_preview_shots;

pub async fn handle_smartcode(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    intros: &IntrosConfig,
) -> Option<Job> {
    let preset = resolve_preset(command, intros);
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
        preset,
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

pub async fn handle_smartcode_exp(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) -> Option<Job> {
    let mut response_msg = working_response(ctx, command, "Working…").await?;
    let result =
        smartcode_merge_upload(ctx, command, &mut response_msg, "/smartcode exp", "smartcode-exp")
            .await?;

    let Some(ts_bytes) = result.ts_bytes.as_ref() else {
        let _ = response_msg
            .edit(
                ctx,
                EditMessage::new().content("Episode has no typeset file to preview."),
            )
            .await;
        return None;
    };

    let ts_path = preview_temp_ass_path(response_msg.id.get());
    if let Err(e) = tokio::fs::write(&ts_path, ts_bytes).await {
        let _ = response_msg
            .edit(
                ctx,
                EditMessage::new().content(format!("Failed to prepare TS preview file: {}", e)),
            )
            .await;
        return None;
    }
    let ts = SubstationAlpha::load(ts_path.clone(), false).await;
    let _ = tokio::fs::remove_file(&ts_path).await;
    let shots = select_preview_shots(&ts, 3, 1000);
    if shots.is_empty() {
        let _ = response_msg
            .edit(
                ctx,
                EditMessage::new().content("TS file has no `\\fn` typeset lines to preview."),
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
        Preset::Dummy(None),
        nyaaise(&result.link),
        result.merged_bytes,
        ctx.clone(),
        final_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    );
    job.preview = Some(PreviewRequest {
        shots: shots
            .into_iter()
            .map(|shot| (shot.centiseconds, shot.label))
            .collect(),
        watermark_font,
    });
    Some(job)
}

fn preview_temp_ass_path(job_id: u64) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("pandora_preview_ts_{}_{}.ass", job_id, nanos))
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
