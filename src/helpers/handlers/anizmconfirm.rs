use super::*;
use pandora_toolchain::lib::publishlog::log_publish;
use pandora_toolchain::lib::http::anizm::{
    episode_not_listed, fetch_publishing_catalog, find_episode_option, find_option, format_episode,
    resolve_translation_option, Anizm, EpisodeCreate, SelectOption, TranslationCreate, VideoCreate,
};
use pandora_toolchain::pnworker::server_config::{read_server_fansub, FansubSite};
use serenity::builder::CreateAutocompleteResponse;

// Anizm players are website embeds, so the Drive link is not published here — only the public
// streaming mirrors, in the order the episode page lists them.
const EMBED_LINK_KEYS: &[&str] = &["byse", "lulustream", "voe"];
const MAX_ANIME_CHOICES: usize = 25;
const MAX_CHOICE_LABEL_CHARS: usize = 100;

pub async fn handle_anizmconfirm_autocomplete(
    ctx: &Context,
    interaction: &serenity::all::CommandInteraction,
) {
    let partial = interaction
        .data
        .autocomplete()
        .filter(|option| option.name == "anime")
        .map(|option| option.value.to_string())
        .unwrap_or_default();
    let mut response = CreateAutocompleteResponse::new();
    match fetch_publishing_catalog().await {
        Ok(catalog) => {
            for option in filter_anime_options(&catalog.anime, &partial) {
                response =
                    response.add_string_choice(anime_choice_label(&option), option.id.to_string());
            }
        }
        Err(e) => eprintln!("[anizmconfirm] anime autocomplete failed: {}", e),
    }
    interaction
        .create_response(ctx, CreateInteractionResponse::Autocomplete(response))
        .await
        .ok();
}

pub async fn handle_anizmconfirm(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let job_id = match option_str(command, "job_id").and_then(|s| s.trim().parse::<u64>().ok()) {
        Some(id) => id,
        None => {
            command_error(ctx, command, "Error: `job_id` must be a numeric job id.").await;
            return;
        }
    };
    log_publish(job_id, "/anizmconfirm", format!("invoked by user {} in channel {}", command.user.id, command.channel_id)).await;
    let episode = match option_f64(command, "episode") {
        Some(episode) if episode > 0.0 => episode,
        _ => {
            command_error(ctx, command, "Error: `episode` must be greater than zero.").await;
            return;
        }
    };
    // Anizm's search has no MyAnimeList id at all, so the anime is never guessed from a title: the
    // operator picks it from the staff panel's own option list and Pandora re-checks that id.
    let anime_id = match option_str(command, "anime")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|id| *id > 0)
    {
        Some(id) => id,
        None => {
            command_error(
                ctx,
                command,
                "Error: select an anime from the Anizm staff panel search results.",
            )
            .await;
            return;
        }
    };
    let server_id = match command_server_id(ctx, command, "/anizmconfirm").await {
        Some(id) => id,
        None => return,
    };
    let fansub_id = match read_server_fansub(server_id, FansubSite::Anizm)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|id| *id > 0)
    {
        Some(id) => id,
        None => {
            command_error(
                ctx,
                command,
                "Error: this server has no Anizm fansub. Set one with `/edit anizm_fansub:`.",
            )
            .await;
            return;
        }
    };
    let episode_type = option_trimmed(command, "type").unwrap_or_else(|| "Normal".to_string());
    let create_episode = option_bool(command, "create_episode").unwrap_or(false);
    let bluray = option_bool(command, "bluray").unwrap_or(false);
    let meta = read_channel_meta(server_id, command.channel_id.get());

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

    let embeds = match embed_targets(command, job_id).await {
        Ok(embeds) => embeds,
        Err(e) => {
            anizm_response(ctx, command, job_id, e).await;
            return;
        }
    };

    let catalog = match fetch_publishing_catalog().await {
        Ok(catalog) => catalog,
        Err(e) => {
            anizm_response(ctx, command, job_id, format!("Failed to load Anizm staff panel: {}", e)).await;
            return;
        }
    };
    let anime = match find_option(&catalog.anime, anime_id) {
        Some(anime) => anime,
        None => {
            anizm_response(
                ctx,
                command,
                job_id,
                format!(
                    "Error: Anizm anime id `{}` is not offered by this account's staff panel.",
                    anime_id
                ),
            )
            .await;
            return;
        }
    };
    let fansub = match find_option(&catalog.fansubs, fansub_id) {
        Some(fansub) => fansub,
        None => {
            anizm_response(
                ctx,
                command,
                job_id,
                format!(
                    "Error: Anizm fansub id `{}` is no longer offered by this account. Reselect it with `/edit anizm_fansub:`.",
                    fansub_id
                ),
            )
            .await;
            return;
        }
    };

    let translator = option_trimmed(command, "translator")
        .or_else(|| credit(&meta.tl))
        .unwrap_or_else(|| fansub.label.clone());
    let encoder =
        option_trimmed(command, "encoder").unwrap_or_else(|| fansub.label.clone());

    let client = match Anizm::from_env() {
        Ok(client) => client,
        Err(e) => {
            anizm_response(ctx, command, job_id, format!("Anizm client error: {}", e)).await;
            return;
        }
    };

    let mut notes = Vec::new();
    let episode_option = match resolve_or_create_episode(
        &client,
        anime_id,
        episode,
        create_episode,
        &EpisodeCreate {
            anime_id,
            number: format_episode(episode),
            episode_type: episode_type.clone(),
            hidden: false,
            title_suffix: String::new(),
            translator: translator.clone(),
            encoder: encoder.clone(),
            fansub_id,
            bluray,
        },
        &mut notes,
    )
    .await
    {
        Ok(option) => option,
        Err(e) => {
            anizm_response(ctx, command, job_id, format!("Error: {}", e)).await;
            return;
        }
    };

    let translation = match resolve_or_create_translation(
        &client,
        anime_id,
        episode_option.id,
        &fansub,
        &TranslationCreate {
            anime_id,
            episode_id: episode_option.id,
            translator: translator.clone(),
            fansub_id,
            encoder: encoder.clone(),
            bluray,
        },
        &mut notes,
    )
    .await
    {
        Ok(translation) => translation,
        Err(e) => {
            anizm_response(ctx, command, job_id, format!("Error: {}", e)).await;
            return;
        }
    };

    let mut published = Vec::new();
    let mut failed = Vec::new();
    for (label, embed) in embeds {
        let request = VideoCreate {
            anime_id,
            episode_id: episode_option.id,
            translation_id: translation.id,
            embed,
            uploader_identifier: None,
        };
        match client.add_video(&request).await {
            Ok(_) => published.push(label),
            Err(e) => failed.push(format!("{}: {}", label, e)),
        }
    }

    anizm_response(
        ctx,
        command,
        job_id,
        summary(
            job_id,
            &anime,
            &episode_option,
            &fansub.label,
            &published,
            &notes,
            &failed,
        ),
    )
    .await;
}

// The episode id is taken from the staff form's own option list. When the requested number is not
// offered, publishing stops unless the caller explicitly asked for the episode to be created, so a
// mistyped number never silently adds a row to Anizm.
async fn resolve_or_create_episode(
    client: &Anizm,
    anime_id: u64,
    episode: f64,
    create: bool,
    request: &EpisodeCreate,
    notes: &mut Vec<String>,
) -> Result<SelectOption, String> {
    let episodes = client.episodes(anime_id).await?;
    if let Some(option) = find_episode_option(&episodes, episode)? {
        return Ok(option);
    }
    if !create {
        return Err(format!(
            "{} Pass `create_episode:true` to create it.",
            episode_not_listed(&episodes, episode)
        ));
    }
    client.add_episode(request).await?;
    let episodes = client.episodes(anime_id).await?;
    let option = find_episode_option(&episodes, episode)
        .map_err(|e| format!("episode creation did not produce a usable episode: {}", e))?
        .ok_or_else(|| {
            format!(
                "episode creation did not produce a usable episode: {}",
                episode_not_listed(&episodes, episode)
            )
        })?;
    notes.push(format!("created episode `{}`", format_episode(episode)));
    Ok(option)
}

// A video attaches to a fansub/translator relation, not to the episode directly. The relation for
// this server's own fansub is created when absent, then re-read so the id comes from the server.
pub(super) async fn resolve_or_create_translation(
    client: &Anizm,
    anime_id: u64,
    episode_id: u64,
    fansub: &SelectOption,
    request: &TranslationCreate,
    notes: &mut Vec<String>,
) -> Result<SelectOption, String> {
    let relations = client.translation_relations(anime_id, episode_id).await?;
    if let Ok(existing) = resolve_translation_option(&relations, &fansub.label) {
        return Ok(existing);
    }
    client.add_translation(request).await?;
    let relations = client.translation_relations(anime_id, episode_id).await?;
    let relation = resolve_translation_option(&relations, &fansub.label)
        .map_err(|e| format!("translation relation was not created: {}", e))?;
    notes.push(format!("created translation relation for `{}`", fansub.label));
    Ok(relation)
}

async fn embed_targets(
    command: &serenity::all::CommandInteraction,
    job_id: u64,
) -> Result<Vec<(String, String)>, String> {
    if let Some(embed) = option_trimmed(command, "embed") {
        return Ok(vec![("embed option".to_string(), embed)]);
    }
    let uploaded = uploaded_links(job_id).await?;
    let embeds = job_embed_links(&uploaded);
    if embeds.is_empty() {
        return Err(
            "Error: job has no public streaming links for Anizm. Pass `embed` to publish one manually."
                .to_string(),
        );
    }
    Ok(embeds)
}

pub(super) fn job_embed_links(uploaded: &serde_json::Value) -> Vec<(String, String)> {
    EMBED_LINK_KEYS
        .iter()
        .filter_map(|key| {
            uploaded
                .get(*key)
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
                .map(|value| ((*key).to_string(), value.to_string()))
        })
        .collect()
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

pub(super) fn credit(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value == "---" {
        None
    } else {
        Some(value.to_string())
    }
}

fn filter_anime_options(options: &[SelectOption], partial: &str) -> Vec<SelectOption> {
    let partial = partial.trim().to_lowercase();
    let mut matches = options
        .iter()
        .filter(|option| {
            partial.is_empty()
                || option.id.to_string() == partial
                || option.label.to_lowercase().contains(&partial)
        })
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        anime_priority(left, &partial)
            .cmp(&anime_priority(right, &partial))
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
            .then_with(|| left.id.cmp(&right.id))
    });
    matches.truncate(MAX_ANIME_CHOICES);
    matches
}

fn anime_priority(option: &SelectOption, partial: &str) -> u8 {
    let label = option.label.to_lowercase();
    if option.id.to_string() == partial || label == partial {
        0
    } else if label.starts_with(partial) {
        1
    } else {
        2
    }
}

fn anime_choice_label(option: &SelectOption) -> String {
    let suffix = format!(" (#{})", option.id);
    let available = MAX_CHOICE_LABEL_CHARS.saturating_sub(suffix.chars().count());
    let label = option.label.chars().take(available).collect::<String>();
    format!("{}{}", label, suffix)
}

fn summary(
    job_id: u64,
    anime: &SelectOption,
    episode: &SelectOption,
    fansub: &str,
    published: &[String],
    notes: &[String],
    failed: &[String],
) -> String {
    let mut lines = vec![format!(
        "{} job `{}` to Anizm `{}` (#{}) episode `{}` as **{}**.",
        if failed.is_empty() {
            "Published"
        } else {
            "Partially published"
        },
        job_id,
        anime.label,
        anime.id,
        episode.label,
        fansub,
    )];
    if !published.is_empty() {
        lines.push(format!("Players: {}", published.join(", ")));
    }
    if !notes.is_empty() {
        lines.push(format!("Notes: {}", notes.join(", ")));
    }
    if !failed.is_empty() {
        lines.push(format!("Failed: {}", failed.join("; ")));
    }
    lines.join("\n")
}

async fn anizm_response(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    job_id: u64,
    content: impl Into<String>,
) {
    let content = content.into();
    log_publish(job_id, "/anizmconfirm", &content).await;
    // The command defers before it talks to the provider, so a lost reply edit leaves the
    // ephemeral stuck on "thinking" with no trace anywhere else. Record it either way.
    if let Err(e) = command
        .edit_response(ctx, EditInteractionResponse::new().content(content))
        .await
    {
        eprintln!("[/anizmconfirm] response edit failed: {}", e);
        log_publish(job_id, "/anizmconfirm", format!("response edit failed: {}", e)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(id: u64, label: &str) -> SelectOption {
        SelectOption {
            id,
            label: label.to_string(),
        }
    }

    #[test]
    fn only_streaming_embeds_are_published_and_placeholders_are_ignored() {
        let uploaded = serde_json::json!({
            "drive": "https://drive.google.com/file/d/abc/view",
            "byse": "https://byse.sx/e/byse",
            "lulustream": "Lulustream Başarısız",
            "voe": "https://voe.example/e/voe"
        });
        assert_eq!(
            job_embed_links(&uploaded),
            vec![
                ("byse".to_string(), "https://byse.sx/e/byse".to_string()),
                ("voe".to_string(), "https://voe.example/e/voe".to_string()),
            ]
        );
    }

    #[test]
    fn anime_autocomplete_matches_labels_and_ids() {
        let options = vec![
            option(187, "Naruto"),
            option(17733, "Naruto: Akaki Yotsuba"),
            option(20, "Bleach"),
        ];
        assert_eq!(filter_anime_options(&options, "naruto").len(), 2);
        assert_eq!(filter_anime_options(&options, "naruto")[0].id, 187);
        assert_eq!(filter_anime_options(&options, "17733")[0].id, 17733);
        assert_eq!(filter_anime_options(&options, "bleach")[0].id, 20);
    }

    #[test]
    fn anime_choice_labels_fit_the_discord_limit_and_keep_the_id() {
        let label = anime_choice_label(&option(187, &"x".repeat(200)));
        assert!(label.chars().count() <= MAX_CHOICE_LABEL_CHARS);
        assert!(label.ends_with("(#187)"));
    }

    #[test]
    fn summary_names_the_resolved_anime_episode_and_fansub() {
        let text = summary(
            9,
            &option(187, "Naruto"),
            &option(57948, "12. Bölüm"),
            "Akira Subs",
            &["byse".to_string()],
            &["created episode `12`".to_string()],
            &[],
        );
        assert!(text.contains("Anizm `Naruto` (#187) episode `12. Bölüm`"), "{}", text);
        assert!(text.contains("Notes: created episode `12`"), "{}", text);
    }
}
