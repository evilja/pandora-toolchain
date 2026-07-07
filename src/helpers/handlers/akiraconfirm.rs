use super::*;
use pandora_toolchain::lib::env::core::get_pandora_env;
use pandora_toolchain::lib::env::standard::{AKIRA_API, AKIRA_INDEX, AKIRA_TOKEN};
use pandora_toolchain::lib::http::hyperkira::{
    AkiraClient, EpisodeCreate, EpisodeLinkWrite, EpisodeListQuery, EpisodeUpdate,
};

pub async fn handle_akiraconfirm(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let job_id = match option_str(command, "job_id").and_then(|s| s.trim().parse::<u64>().ok()) {
        Some(id) => id,
        None => {
            command_error(ctx, command, "Error: `job_id` must be a numeric job id.").await;
            return;
        }
    };
    let episode = match option_i64(command, "episode") {
        Some(n) if n >= 0 => n as f64,
        _ => {
            command_error(ctx, command, "Error: `episode` must be zero or greater.").await;
            return;
        }
    };
    let name = match required_trimmed_option(ctx, command, "name", "Episode name").await {
        Some(s) => s,
        None => return,
    };
    let explicit_slug = option_trimmed(command, "slug");
    let channel_meta = command
        .guild_id
        .map(|server_id| read_channel_meta(server_id.get(), command.channel_id.get()));
    if explicit_slug.is_none() && channel_meta.is_none() {
        command_error(ctx, command, "Error: `slug` is required outside a server.").await;
        return;
    }

    if command
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await
        .is_err()
    {
        return;
    }

    let env = get_pandora_env();
    let akira_api = match env
        .get(AKIRA_API)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        Some(v) => v.to_string(),
        None => {
            akiraconfirm_response(
                ctx,
                command,
                format!("Error: `{}` is not configured.", AKIRA_API),
            )
            .await;
            return;
        }
    };
    let akira_token = match env
        .get(AKIRA_TOKEN)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        Some(v) => v.to_string(),
        None => {
            akiraconfirm_response(
                ctx,
                command,
                format!("Error: `{}` is not configured.", AKIRA_TOKEN),
            )
            .await;
            return;
        }
    };
    let index_base = env
        .get(AKIRA_INDEX)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("https://index.akirasubs.com")
        .to_string();

    let db = match pandora_toolchain::lib::db::core::JobDb::new().await {
        Ok(d) => d,
        Err(e) => {
            akiraconfirm_response(ctx, command, format!("Database error: {}", e)).await;
            return;
        }
    };
    let row = match db.get_job(job_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            akiraconfirm_response(ctx, command, "Error: job not found.").await;
            return;
        }
        Err(e) => {
            akiraconfirm_response(ctx, command, format!("Database error: {}", e)).await;
            return;
        }
    };
    if row.stage != 6 {
        akiraconfirm_response(ctx, command, "Error: job is not uploaded yet.").await;
        return;
    }
    let links_json = match row.uploaded_links {
        Some(v) => v,
        None => {
            akiraconfirm_response(ctx, command, "Error: job has no uploaded links.").await;
            return;
        }
    };
    let uploaded: serde_json::Value = match serde_json::from_str(&links_json) {
        Ok(v) => v,
        Err(e) => {
            akiraconfirm_response(
                ctx,
                command,
                format!("Error: uploaded links JSON is invalid: {}", e),
            )
            .await;
            return;
        }
    };
    let client = match AkiraClient::with_bearer(akira_api, akira_token) {
        Ok(c) => c,
        Err(e) => {
            akiraconfirm_response(ctx, command, format!("Akira client error: {}", e)).await;
            return;
        }
    };
    let fallback_slug = explicit_slug.or_else(|| {
        channel_meta
            .as_ref()
            .and_then(|meta| meta.slug.as_ref())
            .map(|slug| slug.trim())
            .filter(|slug| !slug.is_empty())
            .map(|slug| slug.to_string())
    });
    let slug = match resolve_official_akira_slug(
        &client,
        channel_meta.as_ref().and_then(|meta| meta.mal_id),
        fallback_slug,
    )
    .await
    {
        Ok(slug) => slug,
        Err(e) => {
            akiraconfirm_response(ctx, command, e).await;
            return;
        }
    };
    let folder = option_trimmed(command, "folder").unwrap_or_else(|| slug.clone());
    let links = match akira_episode_links(&uploaded, &index_base, &folder, &name) {
        Ok(v) => v,
        Err(e) => {
            akiraconfirm_response(ctx, command, e).await;
            return;
        }
    };
    let player_embed_url = links.first().map(|link| link.url.clone());
    let goindex_url = links
        .iter()
        .find(|link| link.kind == "goindex_player")
        .map(|link| link.url.clone());
    let episode_exists = match akira_episode_exists(&client, &slug, episode).await {
        Ok(exists) => exists,
        Err(e) => {
            akiraconfirm_response(ctx, command, format!("Akira episode lookup failed: {}", e)).await;
            return;
        }
    };
    if episode_exists {
        let update = EpisodeUpdate {
            title: None,
            player_embed_url: player_embed_url.clone(),
            goindex_url: goindex_url.clone(),
            comments_enabled: None,
            released_at: None,
        };
        if let Err(e) = client.update_episode(&slug, episode, &update).await {
            akiraconfirm_response(ctx, command, format!("Akira episode update failed: {}", e)).await;
            return;
        }
    } else {
        let create = EpisodeCreate {
            episode_number: episode,
            title: Some(name.clone()),
            player_embed_url: player_embed_url.clone(),
            goindex_url: goindex_url.clone(),
            comments_enabled: true,
            released_at: None,
            links: links.clone(),
            staff_credits: Vec::new(),
            source: "discord_bot".to_string(),
            skip_webhook: false,
        };
        if let Err(e) = client.create_episode(&slug, &create).await {
            akiraconfirm_response(ctx, command, format!("Akira episode create failed: {}", e)).await;
            return;
        }
    }
    if let Err(e) = client.replace_episode_links(&slug, episode, &links).await {
        akiraconfirm_response(ctx, command, format!("Akira episode links failed: {}", e)).await;
        return;
    }

    akiraconfirm_response(
        ctx,
        command,
        format!(
            "{} job `{}` to Akira `{}` episode `{}`.",
            if episode_exists { "Updated" } else { "Published" },
            job_id,
            slug,
            episode
        ),
    )
    .await;
}

async fn resolve_official_akira_slug(
    client: &AkiraClient,
    mal_id: Option<u64>,
    fallback_slug: Option<String>,
) -> Result<String, String> {
    if let Some(mal_id) = mal_id {
        return client
            .resolve_anime_by_mal_id(mal_id as i64)
            .await
            .map(|resolved| resolved.slug)
            .map_err(|e| format!("Akira slug resolve failed: {}", e));
    }
    fallback_slug.ok_or_else(|| {
        "Error: this channel has no MAL id or attached anime slug. Provide `slug` or run `/attach`/`/init` first.".to_string()
    })
}

async fn akira_episode_exists(
    client: &AkiraClient,
    slug: &str,
    episode: f64,
) -> pandora_toolchain::lib::http::hyperkira::AkiraResult<bool> {
    let mut page = 1i64;
    loop {
        let res = client
            .anime_episodes(
                slug,
                &EpisodeListQuery {
                    page: Some(page),
                    page_size: Some(100),
                    order: None,
                },
            )
            .await?;
        if res.items.iter().any(|ep| same_episode(ep.episode_number, episode)) {
            return Ok(true);
        }
        if page >= res.pages || res.items.is_empty() {
            return Ok(false);
        }
        page += 1;
    }
}

fn same_episode(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.000_001
}

fn akira_episode_links(
    uploaded: &serde_json::Value,
    index_base: &str,
    folder: &str,
    name: &str,
) -> Result<Vec<EpisodeLinkWrite>, String> {
    let mut out = Vec::new();
    if let Some(url) = link_value(uploaded, "doodstream") {
        out.push(link("doodstream", "Doodstream", url, out.len()));
    }
    if let Some(url) = link_value(uploaded, "lulustream") {
        out.push(link("lulustream", "Lulustream", url, out.len()));
    }
    if let Some(url) = link_value(uploaded, "voe") {
        out.push(link("voe", "Voe", url, out.len()));
    }
    if let Some(url) = link_value(uploaded, "abyss") {
        out.push(link("abyss", "Abyss", url, out.len()));
    }
    if let Some(drive) = link_value(uploaded, "drive") {
        let id = drive_file_id(&drive)
            .ok_or_else(|| "Error: could not parse Drive file id from job link.".to_string())?;
        let url = akira_index_url(index_base, folder, name, &id);
        out.push(link("goindex_player", "Drive", url, out.len()));
    }
    if out.is_empty() {
        return Err("Error: job has no usable uploaded links.".to_string());
    }
    Ok(out)
}

fn link(kind: &str, label: &str, url: String, sort_order: usize) -> EpisodeLinkWrite {
    EpisodeLinkWrite {
        kind: kind.to_string(),
        label: label.to_string(),
        url,
        sort_order: sort_order as i64,
    }
}

fn link_value(uploaded: &serde_json::Value, key: &str) -> Option<String> {
    uploaded
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| is_http_url(s))
        .map(|s| s.to_string())
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn drive_file_id(url: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?:/d/|[?&]id=)([A-Za-z0-9_-]+)").unwrap();
    re.captures(url)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

fn akira_index_url(base: &str, folder: &str, name: &str, id: &str) -> String {
    let base = base.trim_end_matches('/');
    let folder = folder
        .split('/')
        .filter(|part| !part.trim().is_empty())
        .map(path_escape)
        .collect::<Vec<_>>()
        .join("/");
    let filename = if name.rsplit('/').next().unwrap_or(name).contains('.') {
        name.to_string()
    } else {
        format!("{}.mkv", name)
    };
    format!(
        "{}/izle/{}/{}--{}?embed=1",
        base,
        folder,
        path_escape(&filename),
        path_escape(id)
    )
}

fn path_escape(raw: &str) -> String {
    let mut out = String::new();
    for b in raw.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

async fn akiraconfirm_response(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    content: impl Into<String>,
) {
    command
        .edit_response(ctx, EditInteractionResponse::new().content(content.into()))
        .await
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn akira_episode_links_skips_upload_progress_placeholders() {
        let uploaded = serde_json::json!({
            "drive": "https://drive.google.com/file/d/abc123/view?usp=sharing",
            "doodstream": "https://doodstream.com/e/dood",
            "lulustream": "Lulustream Başarısız",
            "voe": "Voe Bekleniyor",
            "abyss": "Abyss 534/946 MB"
        });

        let links = akira_episode_links(&uploaded, "https://index.example.test", "show", "Episode 01")
            .expect("links");
        let urls = links.iter().map(|link| link.url.as_str()).collect::<Vec<_>>();

        assert_eq!(links.len(), 2);
        assert!(urls.contains(&"https://doodstream.com/e/dood"));
        assert!(urls.iter().any(|url| url.starts_with("https://index.example.test/izle/show/Episode%2001.mkv--abc123")));
        assert!(!urls.iter().any(|url| url.contains("Abyss 534/946 MB")));
    }

    #[test]
    fn akira_episode_links_errors_when_no_real_urls_exist() {
        let uploaded = serde_json::json!({
            "drive": "Google 12/100 MB",
            "doodstream": "Doodstream Başarısız",
            "abyss": "Abyss 534/946 MB"
        });

        assert_eq!(
            akira_episode_links(&uploaded, "https://index.example.test", "show", "Episode 01")
                .unwrap_err(),
            "Error: job has no usable uploaded links."
        );
    }
}
