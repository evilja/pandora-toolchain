use super::*;

pub async fn handle_get(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let file_type = match option_str(command, "type") {
        Some("Translation") => "TL",
        Some("Typeset") => "TS",
        _ => {
            command_error(ctx, command, "Error: `type` must be Translation or Typeset.").await;
            return;
        }
    };
    let episode = match positive_u32_option(ctx, command, "episode").await {
        Some(n) => n,
        None => return,
    };
    let server_id = match command_server_id(ctx, command, "/get").await {
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

    let safe_name = meta.name.clone().unwrap_or_default().replace('/', "-");
    let ass_path = format!("{}/{} - {} - E{:02}.ass", pad2(episode), file_type, safe_name, episode);
    let zip_path = format!("{}.zip", ass_path);

    let (path, download_url) = match fg.get_file_download_url(&owner_repo, &zip_path).await {
        Ok(Some(url)) => (zip_path, url),
        Ok(None) => match fg.get_file_download_url(&owner_repo, &ass_path).await {
            Ok(Some(url)) => (ass_path, url),
            Ok(None) => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("{} file not found at `{}` or `{}.zip`.", file_type, ass_path, ass_path))).await;
                return;
            }
            Err(e) => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("Failed to fetch download link: {}", e))).await;
                return;
            }
        },
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to fetch download link: {}", e))).await;
            return;
        }
    };

    let label = match file_type {
        "TL" => "Translation",
        _ => "Typeset",
    };
    let _ = response_msg.edit(ctx, EditMessage::new()
        .content(format!("{} file for episode {}:\n{}\n`{}`", label, episode, download_url, path))).await;
}
