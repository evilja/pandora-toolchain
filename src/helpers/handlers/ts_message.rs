use super::*;

pub async fn handle_ts_message(ctx: &Context, msg: &Message, parts: &[&str]) {
    let episode = match parts.get(1).and_then(|s| s.parse::<u32>().ok()).filter(|n| *n >= 1) {
        Some(n) => n,
        None => {
            msg.reply(ctx, "Usage: `!ts <episode>` with a subtitle or .zip attachment.").await.ok();
            return;
        }
    };
    let attachment = match msg.attachments.first() {
        Some(a) => a,
        None => {
            msg.reply(ctx, "Error: attach a subtitle file (.ass, .srt, .vtt, ...) or a .zip.").await.ok();
            return;
        }
    };
    let server_id = match msg.guild_id {
        Some(g) => g.get(),
        None => {
            msg.reply(ctx, "Error: !ts can only be used in a server.").await.ok();
            return;
        }
    };
    let meta = read_channel_meta(server_id, msg.channel_id.get());
    if meta.mal_id.is_none() {
        msg.reply(ctx, "Error: this channel is not attached to an anime. Run `/init` or `/attach` first.").await.ok();
        return;
    }
    let max_ep = meta.episode_count.unwrap_or(0);
    if episode > max_ep {
        msg.reply(ctx, format!("Error: episode must be between 1 and {}.", max_ep)).await.ok();
        return;
    }
    let repo_url = match meta.repo_url.clone().filter(|s| !s.is_empty()) {
        Some(u) => u,
        None => {
            msg.reply(ctx, "Error: this channel has no repo URL configured.").await.ok();
            return;
        }
    };
    let (owner, repo) = match parse_repo_url(&repo_url) {
        Ok(t) => t,
        Err(e) => {
            msg.reply(ctx, format!("Error: bad repo URL in meta: {}", e)).await.ok();
            return;
        }
    };
    let owner_repo = format!("{}/{}", owner, repo);
    let (_lang, forgejo_base, api_key) = match read_server_meta(server_id).await {
        Ok(t) => t,
        Err(e) => {
            msg.reply(ctx, format!("Error: failed to read server meta: {}", e)).await.ok();
            return;
        }
    };
    if forgejo_base.is_empty() {
        msg.reply(ctx, "Error: server has no git org configured. Run `/configure` first.").await.ok();
        return;
    }
    let mut response_msg = match msg.reply(ctx, "Working...").await {
        Ok(m) => m,
        Err(_) => return,
    };

    let attachment_bytes = match attachment.download().await {
        Ok(b) => b,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to download attachment: {}", e))).await;
            return;
        }
    };

    let job_id = response_msg.id.get();
    println!("[ts] id={} episode={} attachment={} attachment_bytes={}",
        job_id, episode, attachment.filename, attachment_bytes.len());
    let job_dir = format!("DB/saved_data/{}", job_id);
    if let Err(e) = tokio::fs::create_dir_all(&job_dir).await {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to create job dir: {}", e))).await;
        return;
    }
    let input_path = format!("{}/input.ass", job_dir);
    let output_path = format!("{}/output.ass", job_dir);

    let attachment_name = attachment.filename.to_lowercase();
    let (source_name, source_bytes) = if attachment_name.ends_with(".zip") {
        let extract_dir = format!("{}/extract", job_dir);
        if let Err(e) = tokio::fs::create_dir_all(&extract_dir).await {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to create extract dir: {}", e))).await;
            return;
        }
        match extract_zip_root_subtitle(&attachment_bytes, &PathBuf::from(&extract_dir), true).await {
            Ok(Some(src)) => {
                let name = src.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("subtitle")
                    .to_string();
                match tokio::fs::read(&src).await {
                    Ok(b) => (name, b),
                    Err(e) => {
                        let _ = response_msg.edit(ctx, EditMessage::new()
                            .content(format!("Failed to read extracted subtitle: {}", e))).await;
                        return;
                    }
                }
            }
            Ok(None) => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content("Error: zip must contain exactly one subtitle file at the root.")).await;
                return;
            }
            Err(e) => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("Zip extraction failed: {}", e))).await;
                return;
            }
        }
    } else {
        (attachment.filename.clone(), attachment_bytes)
    };

    let mut conversion_warning: Option<String> = None;
    let ass_bytes = match ensure_ass(&source_name, &source_bytes).await {
        Ok(converted) => {
            if let Some(warning) = converted.warning {
                println!("[ts] id={} converted={}", job_id, source_name);
                conversion_warning = Some(warning);
            }
            converted.bytes
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Error: {}", e))).await;
            return;
        }
    };
    if let Err(e) = tokio::fs::write(&input_path, &ass_bytes).await {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to write input: {}", e))).await;
        return;
    }
    println!("[ts] id={} input_ass_bytes={}", job_id, ass_bytes.len());

    if let Err(e) = tokio::fs::copy(&input_path, &output_path).await {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to copy input to output: {}", e))).await;
        return;
    }

    let anime_name = meta.name.clone().unwrap_or_default();
    let title = if anime_name.is_empty() { owner.clone() } else { format!("{} - {}", owner, anime_name) };
    let mut sub = SubstationAlpha::load(PathBuf::from(&output_path), false).await;
    sub.script_info.title = title;
    if sub.dump_to_file(PathBuf::from(&output_path)).await.is_err() {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content("Failed to rewrite ASS title.")).await;
        return;
    }
    let output_bytes = match tokio::fs::read(&output_path).await {
        Ok(b) => b,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to read output: {}", e))).await;
            return;
        }
    };
    println!("[ts] id={} output_ass_bytes={} zip_threshold={}", job_id, output_bytes.len(), ASS_ZIP_THRESHOLD_BYTES);
    let safe_name = anime_name.replace('/', "-");
    let repo_path = format!("{}/TS - {} - E{:02}.ass", pad2(episode), safe_name, episode);
    let fg = match Forgejo::new(forgejo_base, api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Git init failed: {}", e))).await;
            return;
        }
    };
    match upsert_repo_ass(&fg, &owner_repo, &repo_path, &output_bytes, "Typeset").await {
        Ok(uploaded_path) => {
            println!("[ts] id={} uploaded_path={} raw_bytes={}", job_id, uploaded_path, output_bytes.len());
            let mut embed = CreateEmbed::new()
                .title("TS complete")
                .field("Repo", format!("`{}`", owner_repo), true)
                .field("File", format!("`{}`", uploaded_path), true)
                .field("Job", format!("`{}`", job_id), true);
            if let Some(warning) = &conversion_warning {
                embed = embed.field("Warnings", warning, false);
            }
            let _ = response_msg.edit(ctx, EditMessage::new().content("").embed(embed)).await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Upload failed: {}", e))).await;
        }
    }
}
