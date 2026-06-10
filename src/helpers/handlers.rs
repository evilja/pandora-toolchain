use super::*;

pub async fn handle_message(
    context: &Context,
    msg: &Message,
    torrent_url: String,
    preset: Preset,
) -> Option<Job> {
    if msg.attachments.is_empty() {
        msg.reply(context, "Error: Subtitle attachment required").await.ok();
        return None;
    }

    let attachment_bytes = match msg.attachments[0].download().await {
        Ok(b) => b,
        Err(e) => {
            msg.reply(context, format!("Failed to download attachment: {}", e)).await.ok();
            return None;
        }
    };

    let response_msg = match msg.channel_id.send_message(context, CreateMessage::new().content("...")).await {
        Ok(m) => m,
        Err(e) => {
            msg.reply(context, format!("Failed to send response: {}", e)).await.ok();
            return None;
        }
    };

    response_msg.react(context, '❌').await.ok();

    Some(Job::new(
        msg.author.id.get(),
        msg.channel_id.get(),
        response_msg.id.get(),
        JobType::Encode,
        msg.id.get(),
        preset,
        nyaaise(&torrent_url),
        attachment_bytes,
        context.clone(),
        response_msg,
        read_lang(msg.guild_id),
        msg.guild_id.map(|g| g.get()),
    ))
}

pub async fn handle_probe(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
) -> Option<Job> {
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Probing torrent...")
    )).await.ok();

    let response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Probe,
        response_msg.id.get(),
        Preset::Dummy(None),   // irrelevant for probe
        nyaaise(&torrent_url),
        vec![],                // no attachment
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    ))
}

pub async fn handle_backup(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
) -> Option<Job> {
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Backup process will begin shortly after...")
    )).await.ok();

    let response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Backup,
        response_msg.id.get(),
        Preset::Dummy(None),
        nyaaise(&torrent_url),
        vec![],
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    ))
}

pub async fn handle_scrape(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
) -> Option<Job> {
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Scraping...")
    )).await.ok();

    let response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Backup,
        response_msg.id.get(),
        Preset::Dummy(None),
        nyaaise(&torrent_url),
        vec![],
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    ))
}
struct SmartMergeResult {
    link: String,
    merged_bytes: Vec<u8>,
    owner_repo: String,
    release_path: String,
    source_path: String,
    fonts_path: Option<String>,
    warnings: Vec<String>,
}

const ASS_ZIP_THRESHOLD_BYTES: usize = 1_500_000;

async fn smartcode_merge_upload(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    response_msg: &mut Message,
    label: &str,
    log_prefix: &str,
) -> Option<SmartMergeResult> {
    let episode = positive_u32_option(ctx, command, "episode").await?;
    let link_opt = option_trimmed(command, "link");
    let server_id = command_server_id(ctx, command, label).await?;
    let (meta, owner_repo, _repo_url) = attached_repo(ctx, command, server_id, Some(episode)).await?;
    let name = meta.name.clone().unwrap_or_default();
    let (forgejo_base, api_key) = forgejo_config(ctx, command, server_id).await?;
    let fg = match Forgejo::new(forgejo_base, api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Forgejo init failed: {}", e))).await;
            return None;
        }
    };

    let safe_name = name.replace('/', "-");
    let folder = pad2(episode);
    let tl_path = format!("{}/TL - {} - E{:02}.ass", folder, safe_name, episode);
    let ts_path = format!("{}/TS - {} - E{:02}.ass", folder, safe_name, episode);

    let tl_bytes = match read_repo_ass(&fg, &owner_repo, &tl_path).await {
        Ok(Some((b, _))) => b,
        Ok(None) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("TL file not found at `{}` or `{}.zip`.", tl_path, tl_path))).await;
            return None;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to fetch TL: {}", e))).await;
            return None;
        }
    };

    let ts_bytes_opt = match read_repo_ass(&fg, &owner_repo, &ts_path).await {
        Ok(Some((b, _))) => Some(b),
        Ok(None) => None,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to fetch TS: {}", e))).await;
            return None;
        }
    };

    let link = match link_opt {
        Some(ref l) => l.clone(),
        None => {
            let source_md_path = format!("{}/SOURCE.md", folder);
            let b64 = match fg.get_file_content(&owner_repo, &source_md_path).await {
                Ok(Some((b, _))) => b,
                Ok(None) => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("`link` was not provided and no `{}` exists in the repo to read it from.",
                            source_md_path))).await;
                    return None;
                }
                Err(e) => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("Failed to fetch `{}`: {}", source_md_path, e))).await;
                    return None;
                }
            };
            let bytes = match base64_decode_bytes(&b64) {
                Ok(b) => b,
                Err(e) => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("Failed to decode `{}` base64: {}", source_md_path, e))).await;
                    return None;
                }
            };
            let text = match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(e) => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("`{}` is not valid UTF-8: {}", source_md_path, e))).await;
                    return None;
                }
            };
            let parsed = text.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with(';'))
                .map(|l| l.trim_start_matches('#').trim().to_string())
                .filter(|s| !s.is_empty());
            match parsed {
                Some(p) => p,
                None => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("`{}` does not contain a parseable source link.", source_md_path))).await;
                    return None;
                }
            }
        }
    };

    let pnass_path = match get_pandora_env().get(PNASS) {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content("Error: PNASS binary path is not set in DB/config/global/environment/env.pandora.")).await;
            return None;
        }
    };

    let job_id = response_msg.id.get();
    let work_dir = match std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
    {
        Ok(d) => std::env::temp_dir().join(format!("pandora_smartcode_{}", d.as_nanos())),
        Err(_) => std::env::temp_dir().join(format!("pandora_smartcode_{}", job_id)),
    };
    if let Err(e) = tokio::fs::create_dir_all(&work_dir).await {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to create work dir: {}", e))).await;
        return None;
    }

    let tl_local = work_dir.join("tl.ass");
    let ts_local = work_dir.join("ts.ass");
    let merged_local = work_dir.join("merged.ass");

    if let Err(e) = tokio::fs::write(&tl_local, &tl_bytes).await {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to write TL: {}", e))).await;
        return None;
    }
    if let Some(ref b) = ts_bytes_opt {
        if let Err(e) = tokio::fs::write(&ts_local, b).await {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to write TS: {}", e))).await;
            return None;
        }
    }

    let spec: &[CliParam] = if ts_bytes_opt.is_some() { PNASS_MERGE } else { PNASS_MERGE_TL_ONLY };
    let mut paths: HashMap<&str, PathValue> = HashMap::from([
        ("INPUT",  PathValue::from(tl_local.display().to_string())),
        ("OUTPUT", PathValue::from(merged_local.display().to_string())),
    ]);
    if ts_bytes_opt.is_some() {
        paths.insert("MERGE", PathValue::from(ts_local.display().to_string()));
    }

    let mut warnings: Vec<String> = Vec::new();
    let mut proto = Protocol::new(vec![1]);
    let result = run_tool(
        &pnass_path,
        spec,
        &paths,
        job_id,
        &mut proto,
        |data| {
            if data.get(0).and_then(|v| v.as_str()) == Some("4") {
                if let Some(line) = data.get(1).and_then(|v| v.as_str()) {
                    warnings.push(line.to_string());
                }
            }
            None
        },
    ).await;
    if !matches!(result, ToolResult::Success) {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("ASS merge failed (warnings so far: {}).", warnings.len()))).await;
        return None;
    }

    let merged_bytes = match tokio::fs::read(&merged_local).await {
        Ok(b) => b,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to read merged ASS: {}", e))).await;
            return None;
        }
    };
    let merged_sub = SubstationAlpha::load(merged_local.clone(), true).await;
    let font_names = merged_sub.font_names();

    let release_path = format!("{}/Release - {} - E{:02}.ass", folder, safe_name, episode);
    let release_commit = "Smartcode merge".to_string();
    let uploaded_release_path = match upsert_repo_ass(&fg, &owner_repo, &release_path, &merged_bytes, &release_commit).await {
        Ok(uploaded_path) => {
            println!("[{}] uploaded {} ({} bytes)", log_prefix, uploaded_path, merged_bytes.len());
            uploaded_path
        }
        Err(e) => {
            println!("[{}] release upload failed for {}: {}", log_prefix, release_path, e);
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Merged ASS upload to `{}` failed: {}\nEncoding will continue with the local file.",
                    release_path, e))).await;
            return None;
        }
    };

    let source_path = format!("{}/SOURCE.md", folder);
    if link_opt.is_none() {
        println!("[{}] source from {} (skipping rewrite)", log_prefix, source_path);
    } else {
        let source_content = format!("# {}\n", source_link(&link));
        let source_b64 = base64_encode(&source_content);
        let source_commit = "Smartcode source".to_string();
        match fg.upsert_file(&owner_repo, &source_path, &source_b64, &source_commit).await {
            Ok(()) => {
                println!("[{}] uploaded {}", log_prefix, source_path);
            }
            Err(e) => {
                println!("[{}] SOURCE.md upload failed for {}: {}", log_prefix, source_path, e);
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("SOURCE.md upload to `{}` failed: {}\nEncoding will continue with the local file.",
                        source_path, e))).await;
            return None;
        }
    }
    }

    let fonts_path = match upsert_fonts_zip(&fg, &owner_repo, server_id, &folder, &font_names).await {
        Ok(p) => p,
        Err(e) => {
            println!("[{}] fonts.zip upload failed for {}: {}", log_prefix, folder, e);
            None
        }
    };

    println!("[{}] repo={} episode={} tl={} ts_presence={} warnings={} merged_bytes={} release={} source_origin={}",
        log_prefix, owner_repo, episode, tl_path,
        if ts_bytes_opt.is_some() { "present" } else { "absent" },
        warnings.len(), merged_bytes.len(), release_path,
        if link_opt.is_some() { "argument" } else { "SOURCE.md" });

    let _ = tokio::fs::remove_dir_all(&work_dir).await;

    Some(SmartMergeResult {
        link,
        merged_bytes,
        owner_repo,
        release_path: uploaded_release_path,
        source_path,
        fonts_path,
        warnings,
    })
}

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

    Some(Job::new(
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
    ))
}

pub async fn handle_merge(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let mut response_msg = match working_response(ctx, command, "Working…").await {
        Some(m) => m,
        None => return,
    };
    let result = match smartcode_merge_upload(ctx, command, &mut response_msg, "/merge", "merge").await {
        Some(r) => r,
        None => return,
    };
    let embed = CreateEmbed::new()
        .title("Merge complete")
        .field("Repo", format!("`{}`", result.owner_repo), true)
        .field("Release", format!("`{}`", result.release_path), true)
        .field("Source", format!("`{}`", result.source_path), true)
        .field("Fonts", result.fonts_path.unwrap_or_else(|| "None found".to_string()), true)
        .field("Warnings", format_warnings_field(&result.warnings), false);
    let _ = response_msg.edit(ctx, EditMessage::new().content("").embed(embed)).await;
}

pub async fn handle_source(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let episode = match positive_u32_option(ctx, command, "episode").await {
        Some(n) => n,
        None => return,
    };
    let link = match option_trimmed(command, "link") {
        Some(s) => s,
        None => {
            command_error(ctx, command, "Error: `link` is required.").await;
            return;
        }
    };
    let server_id = match command_server_id(ctx, command, "/source").await {
        Some(id) => id,
        None => return,
    };
    let (_meta, owner_repo, _repo_url) = match attached_repo(ctx, command, server_id, Some(episode)).await {
        Some(t) => t,
        None => return,
    };
    let (forgejo_base, api_key) = match forgejo_config(ctx, command, server_id).await {
        Some(t) => t,
        None => return,
    };
    let mut response_msg = match working_response(ctx, command, "Working…").await {
        Some(m) => m,
        None => return,
    };

    let fg = match Forgejo::new(forgejo_base, api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Forgejo init failed: {}", e))).await;
            return;
        }
    };

    let folder = pad2(episode);
    let source_path = format!("{}/SOURCE.md", folder);
    let source_content = format!("# {}\n", source_link(&link));
    let source_b64 = base64_encode(&source_content);
    match fg.upsert_file(&owner_repo, &source_path, &source_b64, "Set source link").await {
        Ok(()) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Wrote `{}` with:\n```\n{}\n```", source_path, source_content.trim_end()))).await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to write `{}`: {}", source_path, e))).await;
        }
    }
}
async fn run_attach_or_init(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    is_init: bool,
) {
    let label = if is_init { "/init" } else { "/attach" };

    let server_id = match command_server_id(ctx, command, label).await {
        Some(id) => id,
        None => return,
    };
    let channel_id = command.channel_id.get();

    let mal_url = match option_trimmed(command, "mal") {
        Some(u) => u.to_string(),
        None => {
            command_error(ctx, command, format!("Error: `mal` is required for {}", label)).await;
            return;
        }
    };

    let repo_arg = option_trimmed(command, "repo");

    if !is_init && repo_arg.is_none() {
        command_error(ctx, command, "Error: `repo` is required for /attach").await;
        return;
    }

    let season_input = option_i64(command, "season").unwrap_or(1);
    if season_input < 1 || season_input > u16::MAX as i64 {
        command_error(ctx, command, "Error: `season` must be between 1 and 65535.").await;
        return;
    }
    let season = season_input as u16;

    let tl = read_credit_option(command, "tl");
    let tlc = read_credit_option(command, "tlc");
    let ts = read_credit_option(command, "ts");
    let qc = read_credit_option(command, "qc");

    let existing = read_channel_meta(server_id, channel_id);

    let (forgejo_base, api_key) = match forgejo_config(ctx, command, server_id).await {
        Some(t) => t,
        None => return,
    };

    let mut response_msg = match working_response(ctx, command, "Working...").await {
        Some(m) => m,
        None => return,
    };

    let meta = match fetch_anime(&mal_url).await {
        Ok(m) => m,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("MAL fetch failed: {}", e))).await;
            return;
        }
    };

    if let Some(eid) = existing.mal_id {
        if eid != meta.mal_id {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!(
                "Channel is already attached to `{}`. Use a different channel to attach a new anime.",
                existing.name.unwrap_or_default()
            ))).await;
            return;
        }
    }

    let fg = match Forgejo::new(forgejo_base.clone(), api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Forgejo init failed: {}", e))).await;
            return;
        }
    };

    let (owner_repo, repo_url) = if is_init {
        let repo_slug = meta.slug.replace('-', "_");
        let or = format!("{}/{}", fg.org, repo_slug);
        let url = match fg.create_repo(&repo_slug).await {
            Ok(u) => u,
            Err(e) => {
                let _ = response_msg.edit(ctx, EditMessage::new().content(format!("create_repo failed: {}", e))).await;
                return;
            }
        };
        (or, url)
    } else {
        let repo_url = repo_arg.unwrap();
        let (owner, repo) = match parse_repo_url(&repo_url) {
            Ok(t) => t,
            Err(e) => {
                let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Bad repo URL: {}", e))).await;
                return;
            }
        };
        (format!("{}/{}", owner, repo), repo_url)
    };

    let existing_root = match fg.list_contents(&owner_repo, "").await {
        Ok(v) => v,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("list_contents failed: {}", e))).await;
            return;
        }
    };

    let episode_count_at_git = count_existing_episodes(&existing_root, meta.episode_count);

    let base_md = tokio::fs::read_to_string(format!("DB/config/{}/base.md", server_id))
        .await
        .ok()
        .map(|t| substitute_base_md(&t, &meta, &repo_url, episode_count_at_git, season, &tl, &tlc, &ts, &qc));

    let created = match bootstrap_repo(&fg, &owner_repo, &meta, base_md, existing_root).await {
        Ok(v) => v,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Bootstrap failed: {}", e))).await;
            return;
        }
    };

    let mut renamed_files: Vec<String> = Vec::new();
    if !is_init {
        let safe_name = meta.name.replace('/', "-");
        for n in 1..=meta.episode_count {
            let folder = pad2(n);
            let entries = match fg.list_contents(&owner_repo, &folder).await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let ass_files: Vec<String> = entries.into_iter()
                .filter(|name| name.to_lowercase().ends_with(".ass"))
                .collect();
            if ass_files.len() != 1 {
                continue;
            }
            let old_name = ass_files.into_iter().next().unwrap();
            let old_path = format!("{}/{}", folder, old_name);
            let new_name = format!("TL - {} - E{:02}.ass", safe_name, n);
            let new_path = format!("{}/{}", folder, new_name);
            if old_path == new_path {
                continue;
            }
            if let Err(e) = fg.move_file(&owner_repo, &old_path, &new_path, "attach: rename to standard filename").await {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("move_file failed ({}): {}", old_path, e))).await;
                return;
            }
            renamed_files.push(format!("`{}` -> `{}`", folder, new_name));
        }
    }

    let new_meta = ChannelMeta {
        mal_id: Some(meta.mal_id),
        kind: Some(kind_label(&meta.kind).to_string()),
        name: Some(meta.name.clone()),
        slug: Some(meta.slug.clone()),
        episode_count: Some(meta.episode_count),
        repo_url: Some(repo_url.clone()),
        episode_count_at_git: Some(episode_count_at_git),
        year: meta.year,
        season: season,
        tl: tl.clone(),
        tlc: tlc.clone(),
        ts: ts.clone(),
        qc: qc.clone(),
    };
    if let Err(e) = write_channel_meta(server_id, channel_id, &new_meta).await {
        let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Failed to save channel meta: {}", e))).await;
        return;
    }

    let created_list = if created.is_empty() {
        "_none — repo already had all folders and README_".to_string()
    } else {
        created.join(", ")
    };
    let renamed = try_rename_channel_to_anime(ctx, command.channel_id, &meta.name).await;
    let rename_line = match &renamed {
        Some(n) => format!("\nChannel renamed: `{}`", n),
        None => String::new(),
    };
    let body = format!(
        "**{}** — attached to this channel.\nName: `{}`\nSlug: `{}`\nKind: `{}`\nEpisodes: `{}`\nRepo: <{}>\nCreated/updated: {}{}",
        label, meta.name, meta.slug, kind_label(&meta.kind), meta.episode_count, repo_url, created_list, rename_line,
    );
    let _ = response_msg.edit(ctx, EditMessage::new().content(body)).await;
}

pub async fn handle_init(ctx: &Context, command: &serenity::all::CommandInteraction) {
    run_attach_or_init(ctx, command, true).await;
}

pub async fn handle_attach(ctx: &Context, command: &serenity::all::CommandInteraction) {
    run_attach_or_init(ctx, command, false).await;
}

pub async fn handle_destruct(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let server_id = match command_server_id(ctx, command, "/destruct").await {
        Some(id) => id,
        None => return,
    };
    let channel_id = command.channel_id.get();

    let (meta, owner_repo, _repo_url) = match attached_repo(ctx, command, server_id, None).await {
        Some(t) => t,
        None => return,
    };
    let anime_name = meta.name.clone().unwrap_or_default();

    let (forgejo_base, api_key) = match forgejo_config(ctx, command, server_id).await {
        Some(t) => t,
        None => return,
    };
    let mut response_msg = match working_response(ctx, command, "Working…").await {
        Some(m) => m,
        None => return,
    };

    let fg = match Forgejo::new(forgejo_base, api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Forgejo init failed: {}", e))).await;
            return;
        }
    };

    match fg.delete_repo(&owner_repo).await {
        Ok(()) => {
            let _ = tokio::fs::remove_file(meta_path(server_id, channel_id)).await;
            let name_line = if anime_name.is_empty() {
                String::new()
            } else {
                format!(" (`{}`)", anime_name)
            };
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Deleted repo `{}`{}.\nChannel detached from this anime.", owner_repo, name_line))).await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("delete_repo failed: {}", e))).await;
        }
    }
}

pub async fn handle_detach(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let server_id = match command_server_id(ctx, command, "/detach").await {
        Some(id) => id,
        None => return,
    };
    let channel_id = command.channel_id.get();

    let meta = read_channel_meta(server_id, channel_id);
    if meta.repo_url.as_deref().map_or(true, str::is_empty) {
        command_error(ctx, command, "Error: this channel is not attached to an anime.").await;
        return;
    }
    let anime_name = meta.name.clone().unwrap_or_default();
    let repo_url = meta.repo_url.clone().unwrap_or_default();

    let mut response_msg = match working_response(ctx, command, "Working…").await {
        Some(m) => m,
        None => return,
    };

    let _ = tokio::fs::remove_file(meta_path(server_id, channel_id)).await;

    let name_line = if anime_name.is_empty() {
        String::new()
    } else {
        format!(" (`{}`)", anime_name)
    };
    let _ = response_msg.edit(ctx, EditMessage::new()
        .content(format!("Detached channel from{}.\nRepo `{}` is left untouched.", name_line, repo_url))).await;
}

enum JobKind { TL, TLC, TS }

fn parse_job_kind(s: &str) -> Option<JobKind> {
    match s {
        "TL" => Some(JobKind::TL),
        "TLC" => Some(JobKind::TLC),
        "TS" => Some(JobKind::TS),
        _ => None,
    }
}

async fn extract_zip_root_ass(bytes: &[u8], dest: &Path) -> Result<Option<PathBuf>, String> {
    use async_zip::base::read::stream::ZipFileReader;
    use futures_lite::io::AsyncReadExt;
    use tokio::io::{AsyncWriteExt, BufReader};

    let tmp = std::env::temp_dir().join(format!("pandora_job_zip_{}.zip",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()));

    let result = async {
        {
            let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| e.to_string())?;
            f.write_all(bytes).await.map_err(|e| e.to_string())?;
            f.sync_all().await.map_err(|e| e.to_string())?;
        }
        let f = tokio::fs::File::open(&tmp).await.map_err(|e| e.to_string())?;
        let mut zip = ZipFileReader::with_tokio(BufReader::new(f));

        let mut found: Option<PathBuf> = None;
        let mut count: usize = 0;

        loop {
            let mut entry = match zip.next_with_entry().await.map_err(|e| format!("zip: {}", e))? {
                Some(e) => e,
                None => break,
            };
            let filename = entry.reader().entry().filename().as_str()
                .map_err(|e| format!("zip filename: {}", e))?
                .to_string();
            let is_root = !filename.contains('/');
            let is_ass = filename.to_lowercase().ends_with(".ass");

            if is_root && is_ass {
                count += 1;
                if count > 1 {
                    return Ok(None);
                }
                let mut data = Vec::new();
                entry.reader_mut().read_to_end(&mut data).await
                    .map_err(|e| format!("zip read: {}", e))?;
                let out_path = dest.join(&filename);
                tokio::fs::write(&out_path, &data).await.map_err(|e| e.to_string())?;
                found = Some(out_path);
            }

            zip = entry.skip().await.map_err(|e| format!("zip skip: {}", e))?;
        }

        Ok(found)
    }.await;

    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

async fn zip_single_ass(entry_name: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    {
        let mut writer = async_zip::base::write::ZipFileWriter::new(&mut out);
        let entry = async_zip::ZipEntryBuilder::new(entry_name.to_string().into(), async_zip::Compression::Deflate);
        writer.write_entry_whole(entry, bytes).await.map_err(|e| e.to_string())?;
        writer.close().await.map_err(|e| e.to_string())?;
    }
    Ok(out)
}

async fn unzip_single_ass(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let dir = std::env::temp_dir().join(format!("pandora_repo_ass_zip_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()));
    let result = async {
        tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
        let extracted = extract_zip_root_ass(bytes, &dir).await?
            .ok_or_else(|| "zip must contain exactly one root .ass file".to_string())?;
        tokio::fs::read(extracted).await.map_err(|e| e.to_string())
    }.await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

async fn read_repo_ass(fg: &Forgejo, owner_repo: &str, ass_path: &str) -> Result<Option<(Vec<u8>, String)>, String> {
    let zip_path = format!("{}.zip", ass_path);
    if let Some((b64, _)) = fg.get_file_content(owner_repo, &zip_path).await? {
        let zip_bytes = base64_decode_bytes(&b64)?;
        let ass_bytes = unzip_single_ass(&zip_bytes).await?;
        return Ok(Some((ass_bytes, zip_path)));
    }
    match fg.get_file_content(owner_repo, ass_path).await? {
        Some((b64, _)) => Ok(Some((base64_decode_bytes(&b64)?, ass_path.to_string()))),
        None => Ok(None),
    }
}

async fn upsert_repo_ass(fg: &Forgejo, owner_repo: &str, ass_path: &str, bytes: &[u8], message: &str) -> Result<String, String> {
    let (upload_path, upload_bytes, alternate_path) = if bytes.len() > ASS_ZIP_THRESHOLD_BYTES {
        let entry_name = Path::new(ass_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("subtitle.ass");
        (format!("{}.zip", ass_path), zip_single_ass(entry_name, bytes).await?, ass_path.to_string())
    } else {
        (ass_path.to_string(), bytes.to_vec(), format!("{}.zip", ass_path))
    };
    println!("[ass-upload] owner_repo={} ass_path={} upload_path={} raw_bytes={} upload_bytes={} zipped={}",
        owner_repo,
        ass_path,
        upload_path,
        bytes.len(),
        upload_bytes.len(),
        upload_path.ends_with(".zip"));
    let b64 = base64_encode_bytes(&upload_bytes);
    fg.upsert_file(owner_repo, &upload_path, &b64, message).await?;
    if let Ok(Some(sha)) = fg.get_file_sha(owner_repo, &alternate_path).await {
        let _ = fg.delete_file(owner_repo, &alternate_path, &sha, &format!("Remove alternate {}", alternate_path)).await;
    }
    Ok(upload_path)
}

async fn zip_files(paths: &[PathBuf]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    {
        let mut writer = async_zip::base::write::ZipFileWriter::new(&mut out);
        let mut used: HashMap<String, usize> = HashMap::new();
        for path in paths {
            let data = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
            let base = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("font")
                .to_string();
            let count = used.entry(base.clone()).or_insert(0);
            let name = if *count == 0 {
                base.clone()
            } else {
                format!("{}-{}", count, base)
            };
            *count += 1;
            let entry = async_zip::ZipEntryBuilder::new(name.into(), async_zip::Compression::Deflate);
            writer.write_entry_whole(entry, &data).await.map_err(|e| e.to_string())?;
        }
        writer.close().await.map_err(|e| e.to_string())?;
    }
    Ok(out)
}

async fn upsert_fonts_zip(fg: &Forgejo, owner_repo: &str, server_id: u64, folder: &str, font_names: &[String]) -> Result<Option<String>, String> {
    let roots = vec![
        PathBuf::from("DB").join("fontconfig").join(server_id.to_string()),
        PathBuf::from("DB").join("fontconfig").join("global"),
    ];
    let font_files = find_fonts_with_roots(font_names, &roots);
    println!("[fonts] owner_repo={} requested={} found={}", owner_repo, font_names.len(), font_files.len());
    for name in font_names {
        println!("[fonts] requested={}", name);
    }
    for path in &font_files {
        println!("[fonts] found={}", path.display());
    }
    if font_files.is_empty() {
        return Ok(None);
    }
    let zip = zip_files(&font_files).await?;
    let b64 = base64_encode_bytes(&zip);
    let path = format!("{}/fonts.zip", folder);
    fg.upsert_file(owner_repo, &path, &b64, "Update fonts").await?;
    Ok(Some(path))
}

async fn extract_zip_to_dir(bytes: &[u8], dest: &Path) -> Result<usize, String> {
    use async_zip::base::read::stream::ZipFileReader;
    use futures_lite::io::AsyncReadExt;
    use tokio::io::{AsyncWriteExt, BufReader};

    let tmp = std::env::temp_dir().join(format!("pandora_font_zip_{}.zip",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()));

    let result = async {
        {
            let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| e.to_string())?;
            f.write_all(bytes).await.map_err(|e| e.to_string())?;
            f.sync_all().await.map_err(|e| e.to_string())?;
        }
        tokio::fs::create_dir_all(dest).await.map_err(|e| e.to_string())?;
        let f = tokio::fs::File::open(&tmp).await.map_err(|e| e.to_string())?;
        let mut zip = ZipFileReader::with_tokio(BufReader::new(f));
        let mut count: usize = 0;

        loop {
            let mut entry = match zip.next_with_entry().await.map_err(|e| format!("zip: {}", e))? {
                Some(e) => e,
                None => break,
            };
            let filename = entry.reader().entry().filename().as_str()
                .map_err(|e| format!("zip filename: {}", e))?
                .to_string();
            let trimmed = filename.trim_matches('/');
            if trimmed.is_empty() {
                zip = entry.skip().await.map_err(|e| format!("zip skip: {}", e))?;
                continue;
            }
            let path = Path::new(trimmed);
            if path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                return Err(format!("zip contains unsafe path: {}", filename));
            }
            let out_path = dest.join(path);
            if filename.ends_with('/') {
                tokio::fs::create_dir_all(&out_path).await.map_err(|e| e.to_string())?;
                zip = entry.skip().await.map_err(|e| format!("zip skip: {}", e))?;
                continue;
            }
            if let Some(parent) = out_path.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
            }
            let mut data = Vec::new();
            entry.reader_mut().read_to_end(&mut data).await
                .map_err(|e| format!("zip read: {}", e))?;
            tokio::fs::write(&out_path, &data).await.map_err(|e| e.to_string())?;
            count += 1;
            zip = entry.skip().await.map_err(|e| format!("zip skip: {}", e))?;
        }

        Ok(count)
    }.await;

    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

fn base64_decode_bytes(input: &str) -> Result<Vec<u8>, String> {
    const ALPH: [u8; 128] = {
        let mut a = [255u8; 128];
        let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0;
        while i < chars.len() {
            a[chars[i] as usize] = i as u8;
            i += 1;
        }
        a
    };
    let cleaned: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if cleaned.len() % 4 != 0 {
        return Err(format!("base64: invalid length {}", cleaned.len()));
    }
    let mut out: Vec<u8> = Vec::with_capacity(cleaned.len() / 4 * 3);
    let mut i = 0;
    while i < cleaned.len() {
        let c0 = cleaned[i];
        let c1 = cleaned[i + 1];
        let c2 = cleaned[i + 2];
        let c3 = cleaned[i + 3];
        let pad2 = c2 == b'=';
        let pad3 = c3 == b'=';
        let v0 = ALPH[c0 as usize];
        let v1 = ALPH[c1 as usize];
        if v0 == 255 || v1 == 255 {
            return Err(format!("base64: invalid char at {}", i));
        }
        if !pad2 {
            let v2 = ALPH[c2 as usize];
            if v2 == 255 {
                return Err(format!("base64: invalid char at {}", i + 2));
            }
            out.push((v0 << 2) | (v1 >> 4));
            if !pad3 {
                let v3 = ALPH[c3 as usize];
                if v3 == 255 {
                    return Err(format!("base64: invalid char at {}", i + 3));
                }
                out.push((v1 << 4) | (v2 >> 2));
                out.push((v2 << 6) | v3);
            } else {
                out.push((v1 << 4) | (v2 >> 2));
            }
        } else {
            out.push((v0 << 2) | (v1 >> 4));
        }
        i += 4;
    }
    Ok(out)
}

fn source_link(link: &str) -> String {
    let trimmed = link.trim();
    let re = Regex::new(r"^(https://nyaa\.(?:si|land))/(?:download|view)/([0-9]+)(?:\.torrent|/torrent)?/?$").unwrap();
    match re.captures(trimmed) {
        Some(caps) => format!("{}/view/{}", caps.get(1).unwrap().as_str(), caps.get(2).unwrap().as_str()),
        None => trimmed.to_string(),
    }
}

pub async fn handle_job(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let job_kind = match option_str(command, "type").and_then(parse_job_kind)
    {
        Some(k) => k,
        None => {
            command_error(ctx, command, "Error: `type` must be TL, TLC, or TS.").await;
            return;
        }
    };

    let episode = match positive_u32_option(ctx, command, "episode").await {
        Some(n) => n,
        None => return,
    };

    let attachment = match option_attachment(command, "subtitle") {
        Some(a) => a,
        None => {
            command_error(ctx, command, "Error: `subtitle` attachment is required.").await;
            return;
        }
    };

    let custom_commit = option_str(command, "commit").unwrap_or("").trim().to_string();
    let server_id = match command_server_id(ctx, command, "/job").await {
        Some(id) => id,
        None => return,
    };
    let (meta, owner_repo, _repo_url) = match attached_repo(ctx, command, server_id, Some(episode)).await {
        Some(t) => t,
        None => return,
    };
    let name = meta.name.clone().unwrap_or_default();
    let owner = owner_repo.split('/').next().unwrap_or("").to_string();
    let (forgejo_base, api_key) = match forgejo_config(ctx, command, server_id).await {
        Some(t) => t,
        None => return,
    };
    let mut response_msg = match working_response(ctx, command, "Working…").await {
        Some(m) => m,
        None => return,
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
    println!("[job] id={} kind={} episode={} attachment={} attachment_bytes={}",
        job_id,
        match job_kind { JobKind::TL => "TL", JobKind::TLC => "TLC", JobKind::TS => "TS" },
        episode,
        attachment.filename,
        attachment_bytes.len());
    let job_dir = format!("DB/saved_data/{}", job_id);
    if let Err(e) = tokio::fs::create_dir_all(&job_dir).await {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to create job dir: {}", e))).await;
        return;
    }
    let input_path = format!("{}/input.ass", job_dir);
    let output_path = format!("{}/output.ass", job_dir);

    let attachment_name = attachment.filename.to_lowercase();
    if attachment_name.ends_with(".ass") {
        if let Err(e) = tokio::fs::write(&input_path, &attachment_bytes).await {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to write input: {}", e))).await;
            return;
        }
    } else if attachment_name.ends_with(".zip") {
        let extract_dir = format!("{}/extract", job_dir);
        if let Err(e) = tokio::fs::create_dir_all(&extract_dir).await {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to create extract dir: {}", e))).await;
            return;
        }
        match extract_zip_root_ass(&attachment_bytes, &PathBuf::from(&extract_dir)).await {
            Ok(Some(src)) => {
                if let Err(e) = tokio::fs::copy(&src, &input_path).await {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("Failed to copy extracted .ass: {}", e))).await;
                    return;
                }
            }
            Ok(None) => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content("Error: zip must contain exactly one .ass file at the root.")).await;
                return;
            }
            Err(e) => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("Zip extraction failed: {}", e))).await;
                return;
            }
        }
    } else {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content("Error: unsupported subtitle file type. Use .ass or .zip.")).await;
        return;
    }
    match tokio::fs::metadata(&input_path).await {
        Ok(m) => println!("[job] id={} input_ass_bytes={}", job_id, m.len()),
        Err(e) => println!("[job] id={} input_ass_metadata_failed={}", job_id, e),
    }

    let mut warnings: Vec<String> = Vec::new();
    if matches!(job_kind, JobKind::TLC) {
        let pnass_path = match get_pandora_env().get(PNASS) {
            Some(p) if !p.is_empty() => p.clone(),
            _ => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content("Error: PNASS binary path is not set in DB/config/global/environment/env.pandora.")).await;
                return;
            }
        };
        let mut proto = Protocol::new(vec![1]);
        let result = run_tool(
            &pnass_path,
            PNASS_LAYER,
            &HashMap::from([
                ("INPUT", PathValue::from(input_path.clone())),
                ("OUTPUT", PathValue::from(output_path.clone())),
            ]),
            job_id,
            &mut proto,
            |data| {
                match data.get(0).and_then(|v| v.as_str()) {
                    Some("4") => {
                        if let Some(line) = data.get(1).and_then(|v| v.as_str()) {
                            warnings.push(line.to_string());
                        }
                    }
                    _ => {}
                }
                None
            },
        ).await;
        if !matches!(result, ToolResult::Success) {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("ASS normalisation failed (warnings so far: {}).", warnings.len()))).await;
            return;
        }
    } else {
        if let Err(e) = tokio::fs::copy(&input_path, &output_path).await {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to copy input to output: {}", e))).await;
            return;
        }
    }

    let title = if name.is_empty() { owner.clone() } else { format!("{} - {}", owner, name) };
    let mut sub = SubstationAlpha::load(PathBuf::from(&output_path), false).await;
    sub.script_info.title = title;
    if sub.dump_to_file(PathBuf::from(&output_path)).await.is_err() {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to rewrite ASS title."))).await;
        return;
    }
    let font_names = sub.font_names();

    let output_bytes = match tokio::fs::read(&output_path).await {
        Ok(b) => b,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to read output: {}", e))).await;
            return;
        }
    };
    println!("[job] id={} output_ass_bytes={} zip_threshold={}", job_id, output_bytes.len(), ASS_ZIP_THRESHOLD_BYTES);
    let (file_type_label, prefix, default_msg) = match job_kind {
        JobKind::TL  => ("TL",  "TL",  "Translation"),
        JobKind::TLC => ("TL",  "TLC", "Edit"),
        JobKind::TS  => ("TS",  "TS",  "Typeset"),
    };
    let commit_msg = if custom_commit.is_empty() {
        default_msg.to_string()
    } else {
        format!("[{}] {}", prefix, custom_commit)
    };
    let safe_name = name.replace('/', "-");
    let file_name = format!("{} - {} - E{:02}.ass",
        file_type_label, safe_name, episode);
    let folder = pad2(episode);
    let repo_path = format!("{}/{}", folder, file_name);

    let fg = match Forgejo::new(forgejo_base, api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Forgejo init failed: {}", e))).await;
            return;
        }
    };
    match upsert_repo_ass(&fg, &owner_repo, &repo_path, &output_bytes, &commit_msg).await {
        Ok(uploaded_path) => {
            println!("[job] id={} uploaded_path={} raw_bytes={}", job_id, uploaded_path, output_bytes.len());
            let fonts_text = match upsert_fonts_zip(&fg, &owner_repo, server_id, &folder, &font_names).await {
                Ok(Some(path)) => path,
                Ok(None) => "None found".to_string(),
                Err(e) => {
                    println!("[job] id={} fonts_upload_failed={}", job_id, e);
                    format!("Failed: {}", e)
                }
            };
            let embed = CreateEmbed::new()
                .title("Job complete")
                .field("Repo", format!("`{}`", owner_repo), true)
                .field("File", format!("`{}`", uploaded_path), true)
                .field("Fonts", format!("`{}`", fonts_text), true)
                .field("Job", format!("`{}`", job_id), true)
                .field("Commit Message", format!("`{}`", commit_msg), false)
                .field("Warnings", format_warnings_field(&warnings), false);
            let _ = response_msg.edit(ctx, EditMessage::new().content("").embed(embed)).await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Upload failed: {}", e))).await;
        }
    }
}

pub async fn handle_ts_message(ctx: &Context, msg: &Message, parts: &[&str]) {
    let episode = match parts.get(1).and_then(|s| s.parse::<u32>().ok()).filter(|n| *n >= 1) {
        Some(n) => n,
        None => {
            msg.reply(ctx, "Usage: `!ts <episode>` with an .ass or .zip attachment.").await.ok();
            return;
        }
    };
    let attachment = match msg.attachments.first() {
        Some(a) => a,
        None => {
            msg.reply(ctx, "Error: attach an .ass or .zip file.").await.ok();
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
    if attachment_name.ends_with(".ass") {
        if let Err(e) = tokio::fs::write(&input_path, &attachment_bytes).await {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to write input: {}", e))).await;
            return;
        }
    } else if attachment_name.ends_with(".zip") {
        let extract_dir = format!("{}/extract", job_dir);
        if let Err(e) = tokio::fs::create_dir_all(&extract_dir).await {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to create extract dir: {}", e))).await;
            return;
        }
        match extract_zip_root_ass(&attachment_bytes, &PathBuf::from(&extract_dir)).await {
            Ok(Some(src)) => {
                if let Err(e) = tokio::fs::copy(&src, &input_path).await {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("Failed to copy extracted .ass: {}", e))).await;
                    return;
                }
            }
            Ok(None) => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content("Error: zip must contain exactly one .ass file at the root.")).await;
                return;
            }
            Err(e) => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("Zip extraction failed: {}", e))).await;
                return;
            }
        }
    } else {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content("Error: unsupported subtitle file type. Use .ass or .zip.")).await;
        return;
    }
    match tokio::fs::metadata(&input_path).await {
        Ok(m) => println!("[ts] id={} input_ass_bytes={}", job_id, m.len()),
        Err(e) => println!("[ts] id={} input_ass_metadata_failed={}", job_id, e),
    }

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
    let font_names = sub.font_names();

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
            let folder = pad2(episode);
            let fonts_text = match upsert_fonts_zip(&fg, &owner_repo, server_id, &folder, &font_names).await {
                Ok(Some(path)) => path,
                Ok(None) => "None found".to_string(),
                Err(e) => {
                    println!("[ts] id={} fonts_upload_failed={}", job_id, e);
                    format!("Failed: {}", e)
                }
            };
            let embed = CreateEmbed::new()
                .title("TS complete")
                .field("Repo", format!("`{}`", owner_repo), true)
                .field("File", format!("`{}`", uploaded_path), true)
                .field("Fonts", format!("`{}`", fonts_text), true)
                .field("Job", format!("`{}`", job_id), true);
            let _ = response_msg.edit(ctx, EditMessage::new().content("").embed(embed)).await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Upload failed: {}", e))).await;
        }
    }
}

fn format_warnings_field(warnings: &[String]) -> String {
    if warnings.is_empty() {
        return "None".to_string();
    }
    const LIMIT: usize = 1000;
    let mut out = String::new();
    let mut count = 0usize;
    for w in warnings {
        let piece = format!("- {}\n", w);
        if out.len() + piece.len() > LIMIT {
            out.push_str(&format!("…and {} more", warnings.len() - count));
            return out;
        }
        out.push_str(&piece);
        count += 1;
    }
    if out.len() > 1024 {
        out.truncate(1021);
        out.push_str("…");
    }
    out
}

pub async fn handle_gitcode(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
    preset: Preset,
) -> Option<Job> {
    let subtitle_url = required_trimmed_option(ctx, command, "subtitle_url", "subtitle_url").await?;

    let normalized = github_blob_to_raw(&subtitle_url);
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to build HTTP client: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return None;
        }
    };

    let attachment_bytes = match client.get(&normalized).send().await {
        Ok(resp) => match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                command.create_response(ctx, CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(format!("Failed to fetch subtitle: {}", e))
                        .ephemeral(true)
                )).await.ok();
                return None;
            }
        },
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to fetch subtitle: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return None;
        }
    };

    let response_msg = working_response(ctx, command, "...").await?;

    response_msg.react(ctx, '❌').await.ok();

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Encode,
        response_msg.id.get(),
        preset,
        nyaaise(&torrent_url),
        attachment_bytes,
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    ))
}

fn github_blob_to_raw(url: &str) -> String {
    let re = Regex::new(r"^https?://github\.com/([^/]+)/([^/]+)/blob/([^/]+)/(.+)$").unwrap();
    if let Some(caps) = re.captures(url) {
        format!("https://raw.githubusercontent.com/{}/{}/{}/{}",
            caps.get(1).unwrap().as_str(),
            caps.get(2).unwrap().as_str(),
            caps.get(3).unwrap().as_str(),
            caps.get(4).unwrap().as_str())
    } else {
        url.to_string()
    }
}

pub async fn handle_configure(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/configure").await {
        Some(id) => id,
        None => return,
    };

    let language = match option_str(command, "language") {
        Some(l) if matches!(l, "EN" | "TR" | "JP") => l.to_string(),
        Some(other) => {
            command_error(ctx, command, format!("Error: language `{}` is not one of EN/TR/JP", other)).await;
            return;
        }
        None => {
            command_error(ctx, command, "Error: language is required").await;
            return;
        }
    };

    let forgejo = match option_str(command, "forgejo") {
        Some(u) if u.is_empty() => String::new(),
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.trim_end_matches('/').to_string(),
        Some(other) => {
            command_error(ctx, command, format!("Error: forgejo `{}` must be an http(s) URL", other)).await;
            return;
        }
        None => String::new(),
    };

    let dir = std::path::PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string());
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to create config dir: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let existing_api_key = std::fs::read_to_string(dir.join("meta.pandora"))
        .ok()
        .and_then(|s| s.lines().nth(3).map(str::to_string))
        .unwrap_or_default();

    let new_api_key = option_str(command, "api_key")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing_api_key)
        .to_string();

    let body = format!("{}\n{}\n{}\n{}\n", language, forgejo, command.channel_id.get(), new_api_key);
    let path = dir.join("meta.pandora");
    if let Err(e) = tokio::fs::write(&path, body).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to write meta.pandora: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let forgejo_display = if forgejo.is_empty() { "(unset)".to_string() } else { format!("`{}`", forgejo) };
    let api_key_display = if new_api_key.is_empty() { "(unset)".to_string() } else { "(set)".to_string() };
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Configured server `{}` — language: {}, forgejo: {}, forgejo api_key: {}, announcement channel: <#{}>",
                server_id, language, forgejo_display, api_key_display, command.channel_id.get()))
            .ephemeral(true)
    )).await.ok();
}

pub async fn handle_addapi(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let key_name = match option_trimmed(command, "key_name") {
        Some(s) => s,
        None => {
            command_error(ctx, command, "Error: `key_name` is required.").await;
            return;
        }
    };
    let token = match option_trimmed(command, "token") {
        Some(s) => s,
        None => {
            command_error(ctx, command, "Error: `token` is required.").await;
            return;
        }
    };

    if key_name.contains('\n') || key_name.contains('\r') || key_name.contains('|') {
        command_error(ctx, command, "Error: `key_name` cannot contain newlines or `|`.").await;
        return;
    }
    if token.contains('\n') || token.contains('\r') {
        command_error(ctx, command, "Error: `token` cannot contain newlines.").await;
        return;
    }

    if let Some(parent) = std::path::Path::new(ENV_PATH).parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to create env dir: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    }

    let existed = match upsert_env(ENV_PATH, &key_name, &token) {
        Ok(updated) => updated,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to write env entry: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let action = if existed { "Updated" } else { "Added" };
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("{} `{}` in `{}`.", action, key_name, ENV_PATH))
            .ephemeral(true)
    )).await.ok();
}

pub async fn handle_font(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/font").await {
        Some(id) => id,
        None => return,
    };

    let attachment = option_attachment(command, "file");
    let has_attachment = attachment.is_some();
    let link = option_trimmed(command, "link");

    if attachment.is_none() && link.is_none() {
        command_error(ctx, command, "Error: provide either `file` or `link`.").await;
        return;
    }

    if command.create_response(ctx, CreateInteractionResponse::Defer(
        CreateInteractionResponseMessage::new().ephemeral(true)
    )).await.is_err() {
        return;
    }

    let zip_bytes = if let Some(a) = attachment {
        let name = a.filename.to_lowercase();
        if !name.ends_with(".zip") {
            font_response(ctx, command, "Error: `file` must be a .zip archive.").await;
            return;
        }
        match a.download().await {
            Ok(b) => b,
            Err(e) => {
                font_response(ctx, command, format!("Failed to download attachment: {}", e)).await;
                return;
            }
        }
    } else {
        let url = link.unwrap();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            font_response(ctx, command, "Error: `link` must be an http(s) URL.").await;
            return;
        }
        let resp = match reqwest::get(&url).await {
            Ok(r) => r,
            Err(e) => {
                font_response(ctx, command, format!("Failed to fetch zip: {}", e)).await;
                return;
            }
        };
        if !resp.status().is_success() {
            font_response(ctx, command, format!("Failed to fetch zip: HTTP {}", resp.status())).await;
            return;
        }
        match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                font_response(ctx, command, format!("Failed to read zip body: {}", e)).await;
                return;
            }
        }
    };

    let dir = std::path::PathBuf::from("DB")
        .join("fontconfig")
        .join(server_id.to_string());
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        font_response(ctx, command, format!("Failed to create font dir: {}", e)).await;
        return;
    }

    let count = match extract_zip_to_dir(&zip_bytes, &dir).await {
        Ok(c) => c,
        Err(e) => {
            font_response(ctx, command, format!("Failed to extract zip: {}", e)).await;
            return;
        }
    };

    let source = if has_attachment { "attachment" } else { "link" };
    font_response(ctx, command, format!("Extracted {} file(s) from {} into `{}`.", count, source, dir.display())).await;
}

async fn font_response(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    content: impl Into<String>,
) {
    command.edit_response(ctx, EditInteractionResponse::new().content(content.into())).await.ok();
}

pub async fn handle_readmebase(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/readmebase").await {
        Some(id) => id,
        None => return,
    };

    let attachment = match option_attachment(command, "file") {
        Some(a) => a,
        None => {
            command_error(ctx, command, "Error: `file` attachment is required.").await;
            return;
        }
    };

    let attachment_bytes = match attachment.download().await {
        Ok(b) => b,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to download attachment: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let dir = std::path::PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string());
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to create config dir: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let path = dir.join("base.md");
    if let Err(e) = tokio::fs::write(&path, &attachment_bytes).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to write base.md: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Set `base.md` for server `{}` ({} bytes, from `{}`).",
                server_id, attachment_bytes.len(), attachment.filename))
            .ephemeral(true)
    )).await.ok();
}

pub async fn handle_auth(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let user_id = match option_trimmed(command, "user_id") {
        Some(s) => s,
        None => {
            command_error(ctx, command, "Error: `user_id` is required.").await;
            return;
        }
    };
    let level = option_str(command, "level")
        .unwrap_or("authorize.pandora")
        .to_string();

    if !has_level_at_least(command.user.id.get(), level_rank(&level)) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Error: your level does not outrank `{}`.", level))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let mut to_add = user_id.clone();
    if add_env(&perm_path(&level), &mut to_add) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Authorized <@{}> at `{}`.", user_id, level))
                .ephemeral(true)
        )).await.ok();
    } else {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to authorize: could not open `{}` for writing.", level))
                .ephemeral(true)
        )).await.ok();
    }
}

pub async fn handle_remove(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let user_id = match option_trimmed(command, "user_id") {
        Some(s) => s,
        None => {
            command_error(ctx, command, "Error: `user_id` is required.").await;
            return;
        }
    };
    let level = match option_trimmed(command, "level") {
        Some(s) => s.to_string(),
        None => {
            command_error(ctx, command, "Error: `level` is required.").await;
            return;
        }
    };

    if !has_level_at_least(command.user.id.get(), level_rank(&level)) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Error: your level does not outrank `{}`.", level))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    match remove_env(&perm_path(&level), &user_id) {
        Ok(true) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Removed <@{}> from `{}`.", user_id, level))
                    .ephemeral(true)
            )).await.ok();
        }
        Ok(false) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("<@{}> was not in `{}`.", user_id, level))
                    .ephemeral(true)
            )).await.ok();
        }
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to remove: {}", e))
                    .ephemeral(true)
            )).await.ok();
        }
    }
}

pub async fn handle_interaction(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
    preset: Preset,
) -> Option<Job> {
    let attachment_bytes = match option_attachment(command, "subtitle") {
        Some(att) => match att.download().await {
            Ok(b) => b,
            Err(e) => {
                command_error(ctx, command, format!("Failed to download attachment: {}", e)).await;
                return None;
            }
        },
        None => {
            command_error(ctx, command, "Error: Subtitle file is required").await;
            return None;
        }
    };

    let response_msg = working_response(ctx, command, "...").await?;

    response_msg.react(ctx, '❌').await.ok();

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Encode,
        response_msg.id.get(),
        preset,
        nyaaise(&torrent_url),
        attachment_bytes,
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    ))
}
