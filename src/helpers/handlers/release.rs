use super::*;

pub async fn handle_release(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let episode = match positive_u32_option(ctx, command, "episode").await {
        Some(n) => n,
        None => return,
    };
    let server_id = match command_server_id(ctx, command, "/release").await {
        Some(id) => id,
        None => return,
    };
    let (meta, owner_repo, _repo_url) = match attached_repo(ctx, command, server_id, Some(episode)).await {
        Some(t) => t,
        None => return,
    };
    let (forgejo_base, api_key) = match forgejo_config(ctx, command, server_id).await {
        Some(t) => t,
        None => return,
    };
    let mut response_msg = match working_response(ctx, command, "Working...").await {
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

    let safe_name = meta.name.clone().unwrap_or_default().replace('/', "-");
    let folder = pad2(episode);
    let release_path = format!("{}/Release - {} - E{:02}.ass", folder, safe_name, episode);
    let release_bytes = match read_repo_ass(&fg, &owner_repo, &release_path).await {
        Ok(Some((b, _))) => b,
        Ok(None) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Release file not found at `{}` or `{}.zip`.", release_path, release_path))).await;
            return;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to fetch release ASS: {}", e))).await;
            return;
        }
    };

    let work_dir = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => std::env::temp_dir().join(format!("pandora_release_{}", d.as_nanos())),
        Err(_) => std::env::temp_dir().join(format!("pandora_release_{}", response_msg.id.get())),
    };
    let local_ass = work_dir.join("release.ass");
    if let Err(e) = tokio::fs::create_dir_all(&work_dir).await {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to create work dir: {}", e))).await;
        return;
    }
    if let Err(e) = tokio::fs::write(&local_ass, &release_bytes).await {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to write release ASS: {}", e))).await;
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
        return;
    }

    let release_sub = SubstationAlpha::load(local_ass, true).await;
    let font_names = release_sub.font_names();
    let fonts_zip = match build_fonts_zip(&owner_repo, server_id, &font_names).await {
        Ok(p) => p,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("fonts.zip build failed for `{}`: {}", folder, e))).await;
            let _ = tokio::fs::remove_dir_all(&work_dir).await;
            return;
        }
    };
    let fonts_field = match fonts_zip {
        Some(zip) => {
            let zip_name = format!("Fonts - {} - E{:02}.zip", safe_name, episode);
            let zip_path = work_dir.join(&zip_name);
            if let Err(e) = tokio::fs::write(&zip_path, &zip).await {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("Failed to write fonts zip: {}", e))).await;
                let _ = tokio::fs::remove_dir_all(&work_dir).await;
                return;
            }
            match upload_release_fonts_to_drive(server_id, &safe_name, &zip_path, &zip_name, &work_dir).await {
                Ok(upload) => format!(
                    "{}\nFolder: `{}`\nFile: `{}`",
                    upload.link,
                    upload.folder_label(),
                    zip_name,
                ),
                Err(e) => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("fonts.zip Drive upload failed for `{}`: {}", folder, e))).await;
                    let _ = tokio::fs::remove_dir_all(&work_dir).await;
                    return;
                }
            }
        }
        None => "No matching local fonts found".to_string(),
    };

    let anisub_field = anisub_release_upload(&meta, &owner_repo, &safe_name, episode, &release_bytes).await;
    let _ = tokio::fs::remove_dir_all(&work_dir).await;

    let embed = CreateEmbed::new()
        .title("Release fonts uploaded")
        .field("Repo", format!("`{}`", owner_repo), true)
        .field("Release", format!("`{}`", release_path), true)
        .field("Fonts", fonts_field, false)
        .field("Requested", format!("`{}`", font_names.len()), true)
        .field("AniSub", anisub_field, false);
    let _ = response_msg.edit(ctx, EditMessage::new().content("").embed(embed)).await;
}

async fn anisub_release_upload(
    meta: &ChannelMeta,
    owner_repo: &str,
    safe_name: &str,
    episode: u32,
    release_bytes: &[u8],
) -> String {
    let key = match get_pandora_env().get(ANISUB).filter(|k| !k.is_empty()).cloned() {
        Some(k) => k,
        None => return "Skipped — `anisub` not set".to_string(),
    };
    let anisub = match AniSub::new(key) {
        Ok(a) => a,
        Err(e) => return format!("Init failed: {}", e),
    };

    let anime_name = meta.name.clone().unwrap_or_default();
    if anime_name.trim().is_empty() {
        return "Skipped — anime name unknown".to_string();
    }

    let anime = match anisub.resolve_anilist(&anime_name).await {
        Ok(Some(m)) => m,
        Ok(None) => return format!("No AniList match for `{}`", anime_name),
        Err(e) => return format!("Search failed: {}", e),
    };

    let release_name = owner_repo.split('/').next().unwrap_or(owner_repo).to_string();
    let zip_entry = format!("Release - {} - E{:02}.ass", safe_name, episode);
    let zip_name = format!("{} - E{:02}.zip", safe_name, episode);
    let zip_bytes = match zip_single_ass(&zip_entry, release_bytes).await {
        Ok(b) => b,
        Err(e) => return format!("Zip failed: {}", e),
    };

    match anisub.upload_subtitle(zip_bytes, &zip_name, anime.media_id, &release_name, episode, &meta.tl, DEFAULT_FPS).await {
        Ok(res) => format!("Uploaded id `{}` — AniList `{}`, release `{}`", res.subtitle_id, anime.media_id, release_name),
        Err(e) => format!("Upload failed: {}", e),
    }
}
