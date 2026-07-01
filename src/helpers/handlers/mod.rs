use super::*;

mod message;
mod probe;
mod backup;
mod smartcode;
mod merge;
mod release;
mod source;
mod get;
mod init;
mod attach;
mod destruct;
mod detach;
mod job;
mod ts_message;
mod gitcode;
mod configure;
mod edit;
mod addapi;
mod gentoken;
mod acixconfirm;
mod acixtemplate;
mod font;
mod readmebase;
mod auth;
mod remove;
mod token;
mod lsauth;
mod changerank;
mod interaction;
mod providers;
mod translation;
#[allow(unused_imports)]
pub use self::message::handle_message;
pub use self::probe::handle_probe;
pub use self::backup::handle_backup;
pub use self::smartcode::handle_smartcode;
pub use self::merge::handle_merge;
pub use self::release::handle_release;
pub use self::source::handle_source;
pub use self::get::handle_get;
pub use self::init::handle_init;
pub use self::attach::handle_attach;
pub use self::destruct::handle_destruct;
pub use self::detach::handle_detach;
pub use self::job::handle_job;
pub use self::ts_message::handle_ts_message;
pub use self::gitcode::handle_gitcode;
pub use self::configure::handle_configure;
pub use self::edit::handle_edit;
pub use self::addapi::handle_addapi;
pub use self::gentoken::handle_gentoken;
pub use self::acixconfirm::handle_acixconfirm;
pub use self::acixtemplate::handle_acixtemplate;
pub use self::font::handle_font;
pub use self::readmebase::handle_readmebase;
pub use self::auth::handle_auth;
pub use self::remove::handle_remove;
pub use self::token::{handle_lstoken, handle_rmtoken};
pub use self::lsauth::handle_lsauth;
pub use self::changerank::handle_changerank;
pub use self::interaction::handle_interaction;
pub use self::providers::handle_providers;
pub use self::translation::{handle_addtranslation, handle_addtranslationall, handle_gettranslation, handle_gettranslationall};

struct SmartMergeResult {
    link: String,
    merged_bytes: Vec<u8>,
    owner_repo: String,
    release_path: String,
    source_path: String,
    gdrive_folder_global: String,
    gdrive_folder_local: String,
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

    let mut ts_bytes_opt = match read_repo_ass(&fg, &owner_repo, &ts_path).await {
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
    let wrap_style = server_wrap_style(server_id);
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
    let split_tl_local = work_dir.join("tl_no_signs.ass");
    let mut warnings: Vec<String> = Vec::new();

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
    } else {
        let mut proto = Protocol::new(vec![1]);
        let split_result = run_tool(
            &pnass_path,
            PNASS_SPLIT_SIGNS,
            &HashMap::from([
                ("INPUT",  PathValue::from(tl_local.display().to_string())),
                ("OUTPUT", PathValue::from(split_tl_local.display().to_string())),
                ("SIGNS",  PathValue::from(ts_local.display().to_string())),
                ("WRAPSTYLE", PathValue::from(wrap_style.clone())),
            ]),
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
        if !matches!(split_result, ToolResult::Success) {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("ASS sign split failed (warnings so far: {}).", warnings.len()))).await;
            return None;
        }
        if tokio::fs::metadata(&ts_local).await.is_ok() {
            let split_tl_bytes = match tokio::fs::read(&split_tl_local).await {
                Ok(b) => b,
                Err(e) => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("Failed to read sign-aware TL: {}", e))).await;
                    return None;
                }
            };
            let sign_bytes = match tokio::fs::read(&ts_local).await {
                Ok(b) => b,
                Err(e) => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("Failed to read generated TS: {}", e))).await;
                    return None;
                }
            };
            if let Err(e) = upsert_repo_ass(&fg, &owner_repo, &tl_path, &split_tl_bytes, "Smartcode move signs from TL").await {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("Failed to update TL after sign split: {}", e))).await;
                return None;
            }
            if let Err(e) = upsert_repo_ass(&fg, &owner_repo, &ts_path, &sign_bytes, "Smartcode move signs to TS").await {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("Failed to upload generated TS: {}", e))).await;
                return None;
            }
            if let Err(e) = tokio::fs::write(&tl_local, &split_tl_bytes).await {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("Failed to write sign-aware TL: {}", e))).await;
                return None;
            }
            warnings.push(format!("Sign lines were moved from `{}` into generated `{}`.", tl_path, ts_path));
            ts_bytes_opt = Some(sign_bytes);
        }
    }

    let spec: &[CliParam] = if ts_bytes_opt.is_some() { PNASS_MERGE } else { PNASS_MERGE_TL_ONLY };
    let mut paths: HashMap<&str, PathValue> = HashMap::from([
        ("INPUT",  PathValue::from(tl_local.display().to_string())),
        ("OUTPUT", PathValue::from(merged_local.display().to_string())),
        ("WRAPSTYLE", PathValue::from(wrap_style.clone())),
    ]);
    if ts_bytes_opt.is_some() {
        paths.insert("MERGE", PathValue::from(ts_local.display().to_string()));
    }

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
    if !ass_has_dialogue(&merged_bytes) {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content("ASS merge produced no dialogue lines; release upload was skipped.")).await;
        return None;
    }

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
                remove_gitkeep_for_path(&fg, &owner_repo, &source_path).await;
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

    println!("[{}] repo={} episode={} tl={} ts_presence={} warnings={} merged_bytes={} release={} source_origin={}",
        log_prefix, owner_repo, episode, tl_path,
        if ts_bytes_opt.is_some() { "present" } else { "absent" },
        warnings.len(), merged_bytes.len(), release_path,
        if link_opt.is_some() { "argument" } else { "SOURCE.md" });

    let _ = tokio::fs::remove_dir_all(&work_dir).await;

    Some(SmartMergeResult {
        link,
        merged_bytes,
        gdrive_folder_global: smartcode_global_drive_folder(&owner_repo, &safe_name),
        gdrive_folder_local: smartcode_local_drive_folder(&safe_name),
        owner_repo,
        release_path: uploaded_release_path,
        source_path,
        warnings,
    })
}

fn smartcode_global_drive_folder(owner_repo: &str, safe_name: &str) -> String {
    let owner = owner_repo.split('/').next().unwrap_or("").trim();
    if owner.is_empty() {
        smartcode_local_drive_folder(safe_name)
    } else {
        format!("{}/{}", drive_folder_component(owner), drive_folder_component(safe_name))
    }
}

fn smartcode_local_drive_folder(safe_name: &str) -> String {
    drive_folder_component(safe_name)
}

fn drive_folder_component(s: &str) -> String {
    s.replace('/', "-").trim().to_string()
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
        acix_template: existing.acix_template,
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
            let is_ass = filename.to_lowercase().ends_with(".ass");

            if is_ass {
                if filename.contains('\\') {
                    return Err(format!("zip contains unsafe .ass path: {}", filename));
                }
                let mut components = Path::new(&filename).components();
                let safe_name = match (components.next(), components.next()) {
                    (Some(std::path::Component::Normal(name)), None) => name.to_owned(),
                    _ => return Err(format!("zip contains unsafe .ass path: {}", filename)),
                };
                count += 1;
                if count > 1 {
                    return Ok(None);
                }
                let mut data = Vec::new();
                entry.reader_mut().read_to_end(&mut data).await
                    .map_err(|e| format!("zip read: {}", e))?;
                let out_path = dest.join(safe_name);
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

fn ass_has_dialogue(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes)
        .lines()
        .any(|line| line.trim_start().starts_with("Dialogue:"))
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
    remove_gitkeep_for_path(fg, owner_repo, &upload_path).await;
    if let Ok(Some(sha)) = fg.get_file_sha(owner_repo, &alternate_path).await {
        let _ = fg.delete_file(owner_repo, &alternate_path, &sha, &format!("Remove alternate {}", alternate_path)).await;
    }
    Ok(upload_path)
}

async fn remove_gitkeep_for_path(fg: &Forgejo, owner_repo: &str, path: &str) {
    let folder = match path.rsplit_once('/') {
        Some((folder, _)) if !folder.is_empty() => folder,
        _ => return,
    };
    let gitkeep = format!("{}/.gitkeep", folder);
    if let Ok(Some(sha)) = fg.get_file_sha(owner_repo, &gitkeep).await {
        let _ = fg.delete_file(owner_repo, &gitkeep, &sha, "Remove .gitkeep").await;
    }
}

async fn zip_files(paths: &[PathBuf]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    {
        let mut writer = async_zip::base::write::ZipFileWriter::new(&mut out);
        let mut used: HashSet<String> = HashSet::new();
        for path in paths {
            let data = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
            let name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("font")
                .to_string();
            if !used.insert(name.clone()) {
                continue;
            }
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
    remove_gitkeep_for_path(fg, owner_repo, &path).await;
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

async fn font_response(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    content: impl Into<String>,
) {
    command.edit_response(ctx, EditInteractionResponse::new().content(content.into())).await.ok();
}
