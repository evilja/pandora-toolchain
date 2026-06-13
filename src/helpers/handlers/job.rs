use super::*;

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
            let embed = CreateEmbed::new()
                .title("Job complete")
                .field("Repo", format!("`{}`", owner_repo), true)
                .field("File", format!("`{}`", uploaded_path), true)
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
