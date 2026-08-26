use super::*;
use pandora_toolchain::lib::publishlog::log_publish;
use pandora_toolchain::lib::http::openanime::{
    Anime, EpisodeSource, OpenAnime, Player, PlayerProvider, Resolutions,
};
use pandora_toolchain::pnworker::server_config::{read_server_fansub, FansubSite};

// Ordered the way the OpenAnime episode page lists sources: the Drive index first, then the public
// streaming mirrors.
const UPLOAD_LINK_KEYS: &[&str] = &["drive", "byse", "lulustream", "voe"];

pub async fn handle_openanimeconfirm(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let job_id = match option_str(command, "job_id").and_then(|s| s.trim().parse::<u64>().ok()) {
        Some(id) => id,
        None => {
            command_error(ctx, command, "Error: `job_id` must be a numeric job id.").await;
            return;
        }
    };
    log_publish(job_id, "/openanimeconfirm", format!("invoked by user {} in channel {}", command.user.id, command.channel_id)).await;
    let episode = match positive_u32_option(ctx, command, "episode").await {
        Some(episode) => episode,
        None => return,
    };
    let season = match option_i64(command, "season") {
        Some(season) if season >= 1 && season <= u32::MAX as i64 => Some(season as u32),
        Some(_) => {
            command_error(ctx, command, "Error: `season` must be a positive integer.").await;
            return;
        }
        None => None,
    };
    let resolutions = match resolutions_option(command) {
        Ok(resolutions) => resolutions,
        Err(e) => {
            command_error(ctx, command, e).await;
            return;
        }
    };
    let server_id = match command_server_id(ctx, command, "/openanimeconfirm").await {
        Some(id) => id,
        None => return,
    };
    let meta = read_channel_meta(server_id, command.channel_id.get());
    let season = match (season, meta.season) {
        (Some(season), _) => season,
        (None, 0) => {
            command_error(
                ctx,
                command,
                "Error: this channel has no attached season. Pass `season` or run `/attach`/`/init` first.",
            )
            .await;
            return;
        }
        (None, channel_season) => channel_season as u32,
    };
    let secure_name = match read_server_fansub(server_id, FansubSite::OpenAnime) {
        Some(name) => name,
        None => {
            command_error(
                ctx,
                command,
                "Error: this server has no OpenAnime fansub. Set one with `/edit openanime_fansub:`.",
            )
            .await;
            return;
        }
    };

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

    let uploaded = match uploaded_links(job_id).await {
        Ok(uploaded) => uploaded,
        Err(e) => {
            openanime_response(ctx, command, job_id, e).await;
            return;
        }
    };

    let client = match OpenAnime::from_env() {
        Ok(client) => client,
        Err(e) => {
            openanime_response(ctx, command, job_id, format!("OpenAnime client error: {}", e)).await;
            return;
        }
    };

    // The stored secure name is re-checked against the account directory: publishing under a name
    // that no longer exists silently creates an orphan source on OpenAnime.
    let fansub = match resolve_fansub_selection(FansubSite::OpenAnime, &secure_name).await {
        Ok(fansub) => fansub,
        Err(e) => {
            openanime_response(ctx, command, job_id, format!("Error: {}", e)).await;
            return;
        }
    };

    let anime = match resolve_anime(&client, &meta, option_trimmed(command, "slug")).await {
        Ok(anime) => anime,
        Err(e) => {
            openanime_response(ctx, command, job_id, e).await;
            return;
        }
    };

    if let Err(e) = client.episode(&anime.slug, season, episode).await {
        openanime_response(
            ctx,
            command,
            job_id,
            format!(
                "Error: OpenAnime has no season {} episode {} for `{}`: {}",
                season, episode, anime.slug, e
            ),
        )
        .await;
        return;
    }

    let (players, skipped) = match plan_players(&uploaded, resolutions) {
        Ok(planned) => planned,
        Err(e) => {
            openanime_response(ctx, command, job_id, e).await;
            return;
        }
    };
    let contributors = option_trimmed(command, "contributors").or_else(|| channel_credits(&meta));

    let mut published = Vec::new();
    let mut failed = Vec::new();
    for (label, player) in players {
        let mut source = match EpisodeSource::new(fansub.value.clone(), player) {
            Ok(source) => source,
            Err(e) => {
                failed.push(format!("{}: {}", label, e));
                continue;
            }
        };
        if let Some(contributors) = &contributors {
            source = source.contributors(contributors.clone());
        }
        match client
            .publish_episode(&anime.slug, season, episode, &source)
            .await
        {
            Ok(_) => published.push(label),
            Err(e) => failed.push(format!("{}: {}", label, e)),
        }
    }

    openanime_response(
        ctx,
        command,
        job_id,
        summary(
            job_id,
            &anime.slug,
            season,
            episode,
            &fansub.display(),
            &published,
            &skipped,
            &failed,
        ),
    )
    .await;
}

// Either the caller names the OpenAnime slug or it is resolved from the channel's MAL id. Both
// paths end at an entry whose `malID` equals the channel's, so an approximate title match can never
// publish onto a different anime.
async fn resolve_anime(
    client: &OpenAnime,
    meta: &ChannelMeta,
    slug: Option<String>,
) -> Result<Anime, String> {
    let mal_id = meta.mal_id;
    if let Some(slug) = slug {
        let anime = client
            .anime(&slug)
            .await
            .map_err(|e| format!("Error: OpenAnime slug `{}` lookup failed: {}", slug, e))?;
        match (mal_id, anime.mal_id) {
            (None, _) => Ok(anime),
            (Some(expected), Some(found)) if found == expected => Ok(anime),
            (Some(expected), Some(found)) => Err(format!(
                "Error: OpenAnime `{}` is MAL id {}, but this channel is attached to MAL id {}.",
                slug, found, expected
            )),
            (Some(expected), None) => Err(format!(
                "Error: OpenAnime `{}` has no MAL id, so it cannot be verified against this channel's MAL id {}.",
                slug, expected
            )),
        }
    } else {
        let mal_id = mal_id.ok_or_else(|| {
            "Error: this channel is not attached to an anime. Provide `slug` or run `/attach`/`/init` first.".to_string()
        })?;
        let title = meta.name.clone().unwrap_or_default();
        client
            .resolve_mal_id(mal_id, &title)
            .await
            .map_err(|e| format!("Error: OpenAnime resolve for MAL id {} failed: {}", mal_id, e))
    }
}

// OpenAnime only accepts the player adapters it documents, so an upload host it cannot embed is
// reported as skipped rather than pushed through a guessed adapter number.
pub(super) fn plan_players(
    uploaded: &serde_json::Value,
    resolutions: Resolutions,
) -> Result<(Vec<(String, Player)>, Vec<String>), String> {
    let mut players = Vec::new();
    let mut skipped = Vec::new();
    for key in UPLOAD_LINK_KEYS {
        let Some(url) = link_value(uploaded, key) else {
            continue;
        };
        let player = if *key == "drive" {
            Player::google_drive(url.clone(), resolutions)
        } else {
            match PlayerProvider::from_url(&url) {
                Some(provider) => Player::new(provider, url.clone()),
                None => {
                    skipped.push(format!("{} (no OpenAnime player adapter)", key));
                    continue;
                }
            }
        };
        match player {
            Ok(player) => players.push(((*key).to_string(), player)),
            Err(e) => skipped.push(format!("{} ({})", key, e)),
        }
    }
    if players.is_empty() {
        return Err(format!(
            "Error: job has no links OpenAnime can publish.{}",
            if skipped.is_empty() {
                String::new()
            } else {
                format!(" Skipped: {}.", skipped.join(", "))
            }
        ));
    }
    Ok((players, skipped))
}

fn resolutions_option(command: &serenity::all::CommandInteraction) -> Result<Resolutions, String> {
    match option_str(command, "resolutions").map(str::trim) {
        None | Some("1080p") => Ok(Resolutions::new(true, false, false)),
        Some("1080p+720p") => Ok(Resolutions::hd()),
        Some("1080p+720p+480p") => Ok(Resolutions::new(true, true, true)),
        Some("720p") => Ok(Resolutions::new(false, true, false)),
        Some("480p") => Ok(Resolutions::new(false, false, true)),
        Some(other) => Err(format!("Error: unknown resolution set `{}`.", other)),
    }
}

pub(super) fn channel_credits(meta: &ChannelMeta) -> Option<String> {
    let credits = [&meta.tl, &meta.tlc, &meta.ts, &meta.qc]
        .into_iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty() && *value != "---")
        .collect::<Vec<_>>();
    if credits.is_empty() {
        None
    } else {
        Some(credits.join(" & "))
    }
}

async fn uploaded_links(job_id: u64) -> Result<serde_json::Value, String> {
    let db = pandora_toolchain::lib::db::core::JobDb::new()
        .await
        .map_err(|e| format!("Database error: {}", e))?;
    let row = db
        .get_job(job_id)
        .await
        .map_err(|e| format!("Database error: {}", e))?
        .ok_or_else(|| "Error: job not found.".to_string())?;
    if row.stage != 6 {
        return Err("Error: job is not uploaded yet.".to_string());
    }
    let links = row
        .uploaded_links
        .ok_or_else(|| "Error: job has no uploaded links.".to_string())?;
    serde_json::from_str(&links)
        .map_err(|e| format!("Error: uploaded links JSON is invalid: {}", e))
}

pub(super) fn link_value(uploaded: &serde_json::Value, key: &str) -> Option<String> {
    uploaded
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
        .map(str::to_string)
}

#[allow(clippy::too_many_arguments)]
fn summary(
    job_id: u64,
    slug: &str,
    season: u32,
    episode: u32,
    fansub: &str,
    published: &[String],
    skipped: &[String],
    failed: &[String],
) -> String {
    let mut lines = vec![format!(
        "{} job `{}` to OpenAnime `{}` S{:02}E{:02} as **{}**.",
        if failed.is_empty() {
            "Published"
        } else {
            "Partially published"
        },
        job_id,
        slug,
        season,
        episode,
        fansub,
    )];
    if !published.is_empty() {
        lines.push(format!("Players: {}", published.join(", ")));
    }
    if !skipped.is_empty() {
        lines.push(format!("Skipped: {}", skipped.join(", ")));
    }
    if !failed.is_empty() {
        lines.push(format!("Failed: {}", failed.join("; ")));
    }
    lines.join("\n")
}

async fn openanime_response(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    job_id: u64,
    content: impl Into<String>,
) {
    let content = content.into();
    log_publish(job_id, "/openanimeconfirm", &content).await;
    // The command defers before it talks to the provider, so a lost reply edit leaves the
    // ephemeral stuck on "thinking" with no trace anywhere else. Record it either way.
    if let Err(e) = command
        .edit_response(ctx, EditInteractionResponse::new().content(content))
        .await
    {
        eprintln!("[/openanimeconfirm] response edit failed: {}", e);
        log_publish(job_id, "/openanimeconfirm", format!("response edit failed: {}", e)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_and_documented_hosts_become_players_and_the_rest_are_skipped() {
        let uploaded = serde_json::json!({
            "drive": "https://drive.google.com/file/d/abc123/view?usp=sharing",
            "lulustream": "https://lulustream.com/e/xyz",
            "byse": "https://byse.sx/e/byse",
            "voe": "Voe Bekleniyor"
        });

        let (players, skipped) = plan_players(&uploaded, Resolutions::hd()).unwrap();
        let labels = players
            .iter()
            .map(|(label, _)| label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["drive", "lulustream"]);
        assert_eq!(players[0].1.number(), 7);
        assert_eq!(players[1].1.number(), 9);
        assert_eq!(skipped, vec!["byse (no OpenAnime player adapter)".to_string()]);
    }

    #[test]
    fn upload_progress_placeholders_never_become_players() {
        let uploaded = serde_json::json!({
            "drive": "Google 12/100 MB",
            "byse": "Byse Başarısız"
        });
        let error = plan_players(&uploaded, Resolutions::hd()).unwrap_err();
        assert!(error.contains("no links OpenAnime can publish"), "{}", error);
    }

    #[test]
    fn requested_resolution_flags_reach_the_drive_player() {
        let uploaded = serde_json::json!({ "drive": "https://drive.google.com/file/d/abc/view" });
        let (players, _) = plan_players(&uploaded, Resolutions::new(true, true, true)).unwrap();
        let value = serde_json::to_value(&players[0].1).unwrap();
        assert_eq!(value["extra"]["resolutions"]["480p"], true);
        assert_eq!(value["extra"]["resolutions"]["1080p"], true);
    }

    #[test]
    fn summary_reports_partial_publishes() {
        let text = summary(
            5,
            "slug",
            1,
            7,
            "Akira Subs",
            &["drive".to_string()],
            &["byse (no OpenAnime player adapter)".to_string()],
            &["lulustream: HTTP 500".to_string()],
        );
        assert!(text.starts_with("Partially published job `5`"), "{}", text);
        assert!(text.contains("Skipped: byse"), "{}", text);
        assert!(text.contains("Failed: lulustream: HTTP 500"), "{}", text);
    }
}
