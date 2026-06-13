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
    let fonts_path = match upsert_fonts_zip(&fg, &owner_repo, server_id, &folder, &font_names).await {
        Ok(p) => p,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("fonts.zip upload failed for `{}`: {}", folder, e))).await;
            let _ = tokio::fs::remove_dir_all(&work_dir).await;
            return;
        }
    };
    let _ = tokio::fs::remove_dir_all(&work_dir).await;

    let embed = CreateEmbed::new()
        .title("Release fonts uploaded")
        .field("Repo", format!("`{}`", owner_repo), true)
        .field("Release", format!("`{}`", release_path), true)
        .field("Fonts", fonts_path.unwrap_or_else(|| "No matching local fonts found".to_string()), true)
        .field("Requested", format!("`{}`", font_names.len()), true);
    let _ = response_msg.edit(ctx, EditMessage::new().content("").embed(embed)).await;
}
