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

    let warnings: Vec<String> = Vec::new();
    let title = if name.is_empty() { owner.clone() } else { format!("{} - {}", owner, name) };
    if let Err(e) = standardise_ass_header_only(&input_path, &output_path, &title).await {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to standardise ASS header: {}", e))).await;
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

async fn standardise_ass_header_only(input_path: &str, output_path: &str, title: &str) -> Result<(), String> {
    let text = tokio::fs::read_to_string(input_path).await.map_err(|e| e.to_string())?;
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let script_idx = lines.iter().position(|l| l.trim().eq_ignore_ascii_case("[Script Info]"));
    if script_idx.is_none() {
        let mut header = vec!["[Script Info]".to_string()];
        header.extend(default_script_info_lines(title, None, None));
        header.push(String::new());
        header.extend(lines);
        tokio::fs::write(output_path, header.join(newline)).await.map_err(|e| e.to_string())?;
        return Ok(());
    }

    let start = script_idx.unwrap();
    let end = lines.iter().enumerate().skip(start + 1)
        .find(|(_, l)| {
            let t = l.trim();
            t.starts_with('[') && t.ends_with(']')
        })
        .map(|(i, _)| i)
        .unwrap_or(lines.len());

    let existing_playres_x = header_u16(&lines[start + 1..end], "playresx").filter(|v| *v != 0);
    let existing_playres_y = header_u16(&lines[start + 1..end], "playresy").filter(|v| *v != 0);
    let defaults = default_script_info_lines(title, existing_playres_x, existing_playres_y);
    let mut present = vec![false; defaults.len()];
    for line in lines.iter_mut().take(end).skip(start + 1) {
        let key = line.split_once(':').map(|(k, _)| k.trim().to_lowercase()).unwrap_or_default();
        if let Some(pos) = defaults.iter().position(|d| d.split_once(':').map(|(k, _)| k.trim().eq_ignore_ascii_case(&key)).unwrap_or(false)) {
            *line = defaults[pos].clone();
            present[pos] = true;
        }
    }
    let missing: Vec<String> = defaults.into_iter().enumerate()
        .filter_map(|(i, line)| if present[i] { None } else { Some(line) })
        .collect();
    for (i, line) in missing.into_iter().enumerate() {
        lines.insert(end + i, line);
    }
    tokio::fs::write(output_path, lines.join(newline)).await.map_err(|e| e.to_string())
}

fn default_script_info_lines(title: &str, playres_x: Option<u16>, playres_y: Option<u16>) -> Vec<String> {
    let x = playres_x.unwrap_or(1920);
    let y = playres_y.unwrap_or(1080);
    vec![
        format!("Title: {}", title),
        "ScriptType: v4.00+".to_string(),
        "WrapStyle: 2".to_string(),
        "ScaledBorderAndShadow: Yes".to_string(),
        format!("PlayResX: {}", x),
        format!("PlayResY: {}", y),
        "YCbCr Matrix: TV.709".to_string(),
        format!("LayoutResX: {}", x),
        format!("LayoutResY: {}", y),
    ]
}

fn header_u16(lines: &[String], key: &str) -> Option<u16> {
    lines.iter().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if k.trim().eq_ignore_ascii_case(key) {
            v.trim().parse().ok()
        } else {
            None
        }
    })
}
