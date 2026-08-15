use super::anizmconfirm::{credit, job_embed_links, resolve_or_create_translation};
use super::fansubs::FansubOption;
use super::openanimeconfirm::{channel_credits, link_value, plan_players};
use super::*;
use pandora_toolchain::lib::db::core::JobDb;
use pandora_toolchain::lib::http::acix::{AliasMatch, AnimeCix, MediaType, SearchHit};
use pandora_toolchain::lib::http::anizm::{
    episode_not_listed, fetch_publishing_catalog, find_episode_option, find_option, Anizm,
    SelectOption, TranslationCreate, VideoCreate,
};
use pandora_toolchain::lib::http::openanime::{
    anime_titles, search_title, tmdb_reference, Anime, EpisodeSource, OpenAnime, Resolutions,
};
use pandora_toolchain::pnworker::acix::{confirm_acix_with_overrides, AcixPending, CreditOverrides};
use pandora_toolchain::pnworker::core::{AcixCredits, AcixPublish};
use serenity::builder::CreateAutocompleteResponse;

const MAX_ANIME_CHOICES: usize = 25;
const MAX_CHOICE_CHARS: usize = 100;
// Enough AnimeciX near-misses to show which id it files the anime under without turning one site's
// line into a catalog dump.
const MAX_ACIX_CANDIDATES: usize = 3;
// Two characters is where a title search stops returning the whole catalog, so shorter input is not
// sent at all instead of making every keystroke a request that cannot be useful.
const MIN_ANIME_SEARCH_CHARS: usize = 2;
// The `<slug>|<title>` separator. Neither an OpenAnime slug nor its titles contain it, and it keeps
// the submitted value self-describing while making a hand-typed title fail to parse.
const ANIME_VALUE_SEPARATOR: char = '|';
// Same sentinel `/edit`'s fansub selectors use: a failed lookup still has to be a selectable choice
// with a non-empty value, or Discord drops the whole autocomplete response.
const LOOKUP_FAILED_VALUE: &str = "__lookup_failed__";
// Anizm's encoder field names the tooling that produced the release, not a person, so it is fixed
// rather than defaulting to the fansub the way the translator credit does.
const ANIZM_ENCODER: &str = "Pandora";

// What the operator picked out of the OpenAnime search. OpenAnime answers a title search but has no
// MyAnimeList lookup, so its slug is the only handle that addresses its catalog exactly; the id the
// other two sites are keyed by is read back off the entry rather than carried in the selection.
struct AnimeSelection {
    slug: String,
    title: String,
}

// What the three sites publish against, resolved once. Searching OpenAnime and reading its
// MyAnimeList id back is what makes both halves exact: OpenAnime is addressed by the slug the
// operator picked instead of by a title guess, and AnimeciX by an id instead of by that title.
struct ResolvedAnime {
    mal_id: i64,
    // The title AnimeciX and Anizm are addressed by — AnimeciX's own catalog name whenever the MAL
    // id reached an entry there, so the record stored for `/acixconfirm` re-resolves the way it
    // always has and Anizm's title match keeps the kind of input it was written against.
    name: String,
    // The AnimeciX title id, resolved here whenever the selection route ran. Carried into the queued
    // record so confirm does not repeat a MyAnimeList search that a TMDB import had to settle.
    acix_id: Option<i64>,
    // Present only for a search selection: the exact catalog entry, so OpenAnime is never asked to
    // re-resolve a slug it already answered.
    openanime: Option<Anime>,
    // Why the MyAnimeList id reached no AnimeciX entry. AnimeciX being unreachable, unconfigured, or
    // simply missing the anime is reported on that site's own line instead of stopping the other two.
    acix_error: Option<String>,
}

// `extra` is the one credit override /publish offers, and it means the same thing everywhere: the
// complete credit line, replacing whatever the channel or the queued record would have supplied.
// Each site keeps that line in its own field — AnimeciX's Extra, OpenAnime's contributors, Anizm's
// translator — so one option edits all three rather than only the site it was named after.
enum CreditOverride {
    Keep,
    Replace(String),
    Clear,
}

impl CreditOverride {
    fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            None => Self::Keep,
            Some("-") => Self::Clear,
            Some(value) => Self::Replace(value.to_string()),
        }
    }

    // `None` means the site gets no credit line at all: AnimeciX stores an empty Extra, OpenAnime
    // sends no contributors, and Anizm falls back to the fansub name because its translation
    // relation is always named.
    fn resolve(&self, fallback: Option<String>) -> Option<String> {
        match self {
            Self::Keep => fallback,
            Self::Replace(value) => Some(value.clone()),
            Self::Clear => None,
        }
    }
}

// One `/publish` can name the fansub a site publishes under instead of using that server's `/edit`
// selection. Naming any site at all makes the whole set exclusive: a site that was not named is not
// published. A release that goes out under a different group on one site is a different release, so
// the two sites nobody mentioned must not keep going out under the server default.
#[derive(Default)]
struct FansubOverrides {
    animecix: Option<FansubOption>,
    openanime: Option<FansubOption>,
    anizm: Option<FansubOption>,
}

// What one site publishes under.
enum SiteFansub<'a> {
    ServerDefault,
    Overridden(&'a FansubOption),
    Excluded,
}

impl FansubOverrides {
    // Every named fansub is resolved against its site's own directory before anything publishes, so
    // a typo or an unreachable directory stops the command instead of publishing two sites and
    // failing the third.
    async fn resolve(command: &serenity::all::CommandInteraction) -> Result<Self, String> {
        let mut overrides = Self::default();
        for site in FansubSite::ALL {
            let Some(value) = option_trimmed(command, site.option_name()) else {
                continue;
            };
            let option = resolve_fansub_selection(site, &value).await?;
            match site {
                FansubSite::AnimeciX => overrides.animecix = Some(option),
                FansubSite::OpenAnime => overrides.openanime = Some(option),
                FansubSite::Anizm => overrides.anizm = Some(option),
            }
        }
        Ok(overrides)
    }

    fn is_active(&self) -> bool {
        self.animecix.is_some() || self.openanime.is_some() || self.anizm.is_some()
    }

    fn site(&self, site: FansubSite) -> SiteFansub<'_> {
        let selected = match site {
            FansubSite::AnimeciX => self.animecix.as_ref(),
            FansubSite::OpenAnime => self.openanime.as_ref(),
            FansubSite::Anizm => self.anizm.as_ref(),
        };
        match selected {
            Some(option) => SiteFansub::Overridden(option),
            None if self.is_active() => SiteFansub::Excluded,
            None => SiteFansub::ServerDefault,
        }
    }
}

fn excluded_outcome(site: FansubSite) -> Outcome {
    Outcome::Skipped(format!(
        "a fansub override is active and `{}:` was not part of it; name one to publish here too",
        site.option_name()
    ))
}

// What a single site publish ended as. Everything is reported, so one site that cannot be published
// never hides the two that could.
enum Outcome {
    Published(String),
    Partial(String),
    Skipped(String),
    Failed(String),
}

impl Outcome {
    fn render(&self, site: &str) -> String {
        let (label, detail) = match self {
            Self::Published(detail) => ("Published", detail),
            Self::Partial(detail) => ("Partially published", detail),
            Self::Skipped(detail) => ("Skipped", detail),
            Self::Failed(detail) => ("Failed", detail),
        };
        if detail.is_empty() {
            format!("**{}** — {}", site, label)
        } else {
            format!("**{}** — {}: {}", site, label, detail)
        }
    }
}

// `/publish` autocompletes the anime against OpenAnime and each fansub override against that site's
// own directory, so the focused option decides which one is searched.
pub async fn handle_publish_autocomplete(
    ctx: &Context,
    interaction: &serenity::all::CommandInteraction,
) {
    let focused = interaction.data.autocomplete();
    let site = focused
        .as_ref()
        .and_then(|option| FansubSite::from_option_name(option.name));
    if let Some(site) = site {
        let partial = focused.map(|option| option.value.to_string()).unwrap_or_default();
        fansub_autocomplete(ctx, interaction, site, &partial).await;
        return;
    }
    let partial = focused
        .filter(|option| option.name == "anime")
        .map(|option| option.value.trim().to_string())
        .unwrap_or_default();
    let mut response = CreateAutocompleteResponse::new();
    if partial.chars().count() >= MIN_ANIME_SEARCH_CHARS {
        match anime_choices(&partial).await {
            Ok(choices) => {
                for (label, value) in choices {
                    response = response.add_string_choice(label, value);
                }
            }
            // A dead search must not render as an empty result, or a typo and an outage look the
            // same to the operator.
            Err(e) => {
                eprintln!("[publish] anime autocomplete failed: {}", e);
                response =
                    response.add_string_choice(lookup_failed_label(&e), LOOKUP_FAILED_VALUE);
            }
        }
    }
    interaction
        .create_response(ctx, CreateInteractionResponse::Autocomplete(response))
        .await
        .ok();
}

async fn anime_choices(partial: &str) -> Result<Vec<(String, String)>, String> {
    let client = OpenAnime::catalog()?;
    let hits = client.search(partial).await?;
    Ok(hits
        .into_iter()
        // A result nobody can read, or whose slug leaves no room for a title beside it in the
        // 100-character value, is dropped rather than offered as a choice Discord would reject.
        .filter(|hit| {
            let slug = hit.slug.trim();
            !slug.is_empty() && slug.chars().count() < MAX_CHOICE_CHARS
        })
        .filter_map(|hit| search_title(&hit).map(|title| (hit.slug, title)))
        .take(MAX_ANIME_CHOICES)
        .map(|(slug, title)| {
            (
                anime_choice_label(&title, &slug),
                anime_choice_value(&title, &slug),
            )
        })
        .collect())
}

// Two OpenAnime entries can share a title, so the slug that tells them apart stays on the label.
fn anime_choice_label(title: &str, slug: &str) -> String {
    let suffix = truncate_chars(&format!(" ({})", slug), MAX_CHOICE_CHARS);
    let available = MAX_CHOICE_CHARS.saturating_sub(suffix.chars().count());
    format!("{}{}", truncate_chars(title, available), suffix)
}

// The slug is never truncated — it is the handle the entry is fetched by. The title rides along only
// to keep the value self-describing; the full titles come off the entry itself at submit time.
fn anime_choice_value(title: &str, slug: &str) -> String {
    let prefix = format!("{}{}", slug, ANIME_VALUE_SEPARATOR);
    let available = MAX_CHOICE_CHARS.saturating_sub(prefix.chars().count());
    format!("{}{}", prefix, truncate_chars(title, available))
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn lookup_failed_label(error: &str) -> String {
    const PREFIX: &str = "⚠ OpenAnime lookup failed: ";
    let available = MAX_CHOICE_CHARS.saturating_sub(PREFIX.chars().count());
    let reason = error.split_whitespace().collect::<Vec<_>>().join(" ");
    if reason.chars().count() <= available {
        return format!("{}{}", PREFIX, reason);
    }
    format!(
        "{}{}…",
        PREFIX,
        truncate_chars(&reason, available.saturating_sub(1))
    )
}

// The submitted value is the choice's own payload, never a free-typed title: a hand-written name
// carries no OpenAnime slug and would leave every site guessing.
fn parse_anime_option(value: &str) -> Result<AnimeSelection, String> {
    let (slug, title) = value
        .split_once(ANIME_VALUE_SEPARATOR)
        .ok_or_else(|| "Error: pick `anime` from the search results.".to_string())?;
    let slug = slug.trim();
    let title = title.trim();
    if slug.is_empty() || title.is_empty() {
        return Err("Error: pick `anime` from the search results.".to_string());
    }
    Ok(AnimeSelection {
        slug: slug.to_string(),
        title: title.to_string(),
    })
}

// OpenAnime carries the MyAnimeList id but cannot be searched by it, and AnimeciX cannot be searched
// without a title its own catalog uses. Reading the id off the picked OpenAnime entry and searching
// AnimeciX under every alias that entry lists turns both lookups into exact ones. AnimeciX not
// answering is carried rather than raised, so it never takes the other two sites down with it.
async fn resolve_selection(selection: &AnimeSelection) -> Result<ResolvedAnime, String> {
    let anime = OpenAnime::catalog()?
        .anime(&selection.slug)
        .await
        .map_err(|e| format!("Error: OpenAnime has no entry `{}`: {}", selection.slug, e))?;
    let Some(mal_id) = anime
        .mal_id
        .and_then(|id| i64::try_from(id).ok())
        .filter(|id| *id > 0)
    else {
        return Err(format!(
            "Error: OpenAnime's `{}` records no MyAnimeList id, so AnimeciX cannot be resolved from it. Publish OpenAnime alone with `/openanimeconfirm`.",
            selection.slug
        ));
    };
    let titles = anime_titles(&anime);
    let hit = match AnimeCix::from_token_env() {
        Ok(client) => client.resolve_by_mal_id_aliases(&titles, mal_id).await,
        Err(e) => Err(e),
    };
    let (name, acix_id, acix_error) = match hit {
        Ok(AliasMatch::Found(hit)) => (hit.name, Some(hit.acix_id), None),
        // The MyAnimeList ids disagree, which is the ordinary case rather than a broken one, so the
        // id the two catalogs are actually built from settles it instead.
        Ok(AliasMatch::NotFound(candidates)) => match resolve_by_tmdb(&anime).await {
            Ok(Some(acix_id)) => (fallback_name(&titles, selection), Some(acix_id), None),
            Ok(None) => (
                fallback_name(&titles, selection),
                None,
                Some(acix_missing(mal_id, &titles, &candidates, None)),
            ),
            Err(e) => (
                fallback_name(&titles, selection),
                None,
                Some(acix_missing(mal_id, &titles, &candidates, Some(&e))),
            ),
        },
        Err(e) => (fallback_name(&titles, selection), None, Some(e)),
    };
    Ok(ResolvedAnime {
        mal_id,
        name,
        acix_id,
        openanime: Some(anime),
        acix_error,
    })
}

// AnimeciX's own import resolves a TMDB id to the title it files it under, so it answers the one
// question its title search cannot: which entry is this, when the MyAnimeList ids disagree. The
// import is idempotent by provider id — an anime AnimeciX already carries comes back as that entry —
// but it is still a write, so it runs only after the read-only search has failed. It is also how the
// title gets created at all when AnimeciX does not carry it yet, which is exactly what a publish of
// a new season needs.
async fn resolve_by_tmdb(anime: &Anime) -> Result<Option<i64>, String> {
    let Some((tmdb_id, is_movie)) = tmdb_reference(anime) else {
        return Ok(None);
    };
    let media_type = if is_movie {
        MediaType::Movies
    } else {
        MediaType::Series
    };
    AnimeCix::from_env()?
        .tmdb_to_acix(&tmdb_id, media_type)
        .await
        .map(|result| result.acix_id)
}

// AnimeciX never answered, so Anizm and the reply header fall back to the entry's own leading title.
fn fallback_name(titles: &[String], selection: &AnimeSelection) -> String {
    titles
        .first()
        .cloned()
        .unwrap_or_else(|| selection.title.clone())
}

// The usual reason a MyAnimeList id reaches no AnimeciX entry is not that AnimeciX lacks the anime
// but that it files it under a different id — one entry covering several seasons, or an id that
// belongs to something else entirely. Naming what its search did return is the difference between an
// operator who can see that in one line and one who goes looking for an outage.
fn acix_missing(
    mal_id: i64,
    titles: &[String],
    candidates: &[SearchHit],
    import_error: Option<&str>,
) -> String {
    // The TMDB import is what should have settled the id disagreement, so why it did not is the
    // useful half of this report — the title search missing is only how it got here.
    let import = match import_error {
        Some(e) => format!(" and its TMDB import failed ({})", e),
        None => ", and OpenAnime records no TMDB id to import it by".to_string(),
    };
    let searched = titles
        .iter()
        .map(|title| format!("`{}`", title))
        .collect::<Vec<_>>()
        .join(", ");
    if candidates.is_empty() {
        return format!(
            "no AnimeciX entry for MAL id {}, its search returns nothing under {}{}",
            mal_id, searched, import
        );
    }
    let near = candidates
        .iter()
        .take(MAX_ACIX_CANDIDATES)
        .map(|hit| match hit.mal_id {
            Some(id) => format!("`{}` (MAL {})", hit.name, id),
            None => format!("`{}` (no MAL id)", hit.name),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "no AnimeciX entry for MAL id {} under {} — its search returns {}, so the two catalogs disagree on the id{}",
        mal_id, searched, near, import
    )
}

pub async fn handle_publish(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let job_id = match option_str(command, "job_id").and_then(|s| s.trim().parse::<u64>().ok()) {
        Some(id) => id,
        None => {
            command_error(ctx, command, "Error: `job_id` must be a numeric job id.").await;
            return;
        }
    };
    let season_option = match option_i64(command, "season") {
        Some(season) if season >= 1 => Some(season),
        Some(_) => {
            command_error(ctx, command, "Error: `season` must be a positive integer.").await;
            return;
        }
        None => None,
    };
    let episode_option = match option_i64(command, "episode") {
        Some(episode) if episode >= 1 => Some(episode),
        Some(_) => {
            command_error(ctx, command, "Error: `episode` must be a positive integer.").await;
            return;
        }
        None => None,
    };
    let selected = match option_trimmed(command, "anime").map(|value| parse_anime_option(&value)) {
        Some(Ok(selection)) => Some(selection),
        Some(Err(e)) => {
            command_error(ctx, command, e).await;
            return;
        }
        None => None,
    };
    let extra = option_trimmed(command, "extra");
    let credits = CreditOverride::parse(extra.as_deref());
    let server_id = match command_server_id(ctx, command, "/publish").await {
        Some(id) => id,
        None => return,
    };
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

    let db = match JobDb::new().await {
        Ok(db) => db,
        Err(e) => {
            publish_response(ctx, command, format!("Database error: {}", e)).await;
            return;
        }
    };
    let row = match db.get_job(job_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            publish_response(ctx, command, "Error: job not found.").await;
            return;
        }
        Err(e) => {
            publish_response(ctx, command, format!("Database error: {}", e)).await;
            return;
        }
    };
    if row.stage != 6 {
        publish_response(ctx, command, "Error: job is not uploaded yet.").await;
        return;
    }
    let uploaded: serde_json::Value = match row.uploaded_links.as_deref() {
        Some(links) => match serde_json::from_str(links) {
            Ok(value) => value,
            Err(e) => {
                publish_response(
                    ctx,
                    command,
                    format!("Error: uploaded links JSON is invalid: {}", e),
                )
                .await;
                return;
            }
        },
        None => {
            publish_response(ctx, command, "Error: job has no uploaded links.").await;
            return;
        }
    };

    let overrides = match FansubOverrides::resolve(command).await {
        Ok(overrides) => overrides,
        Err(e) => {
            publish_response(ctx, command, format!("Error: {}", e)).await;
            return;
        }
    };

    // A smartcode job queued its own AnimeciX record at upload time, so it already carries the
    // anime, season, and episode this command would otherwise have to ask for.
    let mut notes = Vec::new();
    let pending: Option<AcixPending> = match row.acix_pending.as_deref() {
        Some(json) => match serde_json::from_str(json) {
            Ok(pending) => Some(pending),
            Err(e) => {
                notes.push(format!("stored AnimeciX publish state is unreadable ({})", e));
                None
            }
        },
        None => None,
    };

    let resolved = match &selected {
        Some(selection) => match resolve_selection(selection).await {
            Ok(resolved) => resolved,
            Err(e) => {
                publish_response(ctx, command, e).await;
                return;
            }
        },
        // Nothing was picked, so the anime comes from what was already recorded and each site keeps
        // resolving it the way it did before this command existed.
        None => {
            let (mal_id, name) = match pending.as_ref() {
                Some(pending) => (pending.acix.mal_id, pending.acix.name.clone()),
                None => match (meta.mal_id, meta.name.clone()) {
                    (Some(mal_id), Some(name)) => (mal_id as i64, name),
                    _ => {
                        publish_response(
                            ctx,
                            command,
                            "Error: this job did not record an anime and this channel is not attached to one. Pass `anime`.",
                        )
                        .await;
                        return;
                    }
                },
            };
            // Both MAL-keyed sites address the anime by this id, so a job or channel that recorded a
            // placeholder is stopped here rather than resolving onto whatever the id wraps to.
            if mal_id <= 0 {
                publish_response(
                    ctx,
                    command,
                    format!(
                        "Error: `{}` is recorded with MAL id {}, which no site can resolve. Pass `anime`.",
                        name, mal_id
                    ),
                )
                .await;
                return;
            }
            ResolvedAnime {
                mal_id,
                name,
                acix_id: None,
                openanime: None,
                acix_error: None,
            }
        }
    };
    let ResolvedAnime {
        mal_id,
        name,
        acix_id,
        openanime,
        acix_error,
    } = resolved;
    let season = season_option
        .or_else(|| pending.as_ref().and_then(|pending| pending.acix.season_num))
        .or(if meta.season >= 1 {
            Some(meta.season as i64)
        } else {
            None
        })
        .unwrap_or(1);
    let episode = match episode_option
        .or_else(|| pending.as_ref().and_then(|pending| pending.acix.episode_num))
    {
        Some(episode) => episode,
        None => {
            publish_response(
                ctx,
                command,
                "Error: this job did not record an episode number. Pass `episode`.",
            )
            .await;
            return;
        }
    };

    // The queued AnimeciX record is published exactly as it was queued, so a season/episode typed
    // here reaches only the other two sites. Saying so beats letting the three quietly disagree.
    if let Some(pending) = pending.as_ref() {
        let queued_season = pending.acix.season_num;
        let queued_episode = pending.acix.episode_num;
        if queued_season.is_some_and(|queued| queued != season)
            || queued_episode.is_some_and(|queued| queued != episode)
        {
            notes.push(format!(
                "AnimeciX publishes its queued S{:02}E{:02}; reset it with `/acixunpublish` to change it",
                queued_season.unwrap_or(season),
                queued_episode.unwrap_or(episode),
            ));
        }
    }

    let acix = publish_acix(
        &db,
        job_id,
        server_id,
        &uploaded,
        &meta,
        mal_id,
        &name,
        season,
        episode,
        extra,
        pending.is_some(),
        acix_id,
        acix_error,
        overrides.site(FansubSite::AnimeciX),
    )
    .await;
    let openanime = publish_openanime(
        server_id,
        &uploaded,
        &meta,
        &credits,
        mal_id,
        &name,
        openanime.as_ref(),
        season,
        episode,
        overrides.site(FansubSite::OpenAnime),
    )
    .await;
    let anizm = publish_anizm(
        server_id,
        &uploaded,
        &meta,
        &credits,
        &name,
        episode,
        overrides.site(FansubSite::Anizm),
    )
    .await;

    let mut lines = vec![format!(
        "Publish of job `{}` — **{}** (MAL {}) S{:02}E{:02}",
        job_id, name, mal_id, season, episode
    )];
    lines.push(acix.render("AnimeciX"));
    lines.push(openanime.render("OpenAnime"));
    lines.push(anizm.render("Anizm"));
    for note in &notes {
        lines.push(format!("_Note: {}_", note));
    }
    publish_response(ctx, command, lines.join("\n")).await;
}

// AnimeciX publishes from the record queued at upload time. A job that never queued one (anything
// that was not a smartcode) gets an equivalent record built from the resolved anime and this
// server's fansub template, so the same confirm path runs for both. An overridden fansub replaces
// that template on either route — on the queued record it is a pre-publish edit, so the confirm
// path refuses it once a half has already reached AnimeciX.
#[allow(clippy::too_many_arguments)]
async fn publish_acix(
    db: &JobDb,
    job_id: u64,
    server_id: u64,
    uploaded: &serde_json::Value,
    meta: &ChannelMeta,
    mal_id: i64,
    name: &str,
    season: i64,
    episode: i64,
    extra: Option<String>,
    has_pending: bool,
    acix_id: Option<i64>,
    unresolved: Option<String>,
    fansub: SiteFansub<'_>,
) -> Outcome {
    let overridden = match fansub {
        SiteFansub::Excluded => return excluded_outcome(FansubSite::AnimeciX),
        SiteFansub::ServerDefault => None,
        SiteFansub::Overridden(option) => {
            match option.value.trim().parse::<i64>().ok().filter(|id| *id > 0) {
                Some(template) => Some((template, option.display())),
                None => {
                    return Outcome::Failed(format!(
                        "`{}` is not an AnimeciX fansub template id",
                        option.value
                    ))
                }
            }
        }
    };
    // The selection's MyAnimeList id already failed to reach an AnimeciX entry, so nothing is queued
    // here: a record built on a title AnimeciX does not carry would only fail again at confirm time,
    // and `/acixunpublish` would then be needed to clear it.
    if let Some(reason) = unresolved {
        return Outcome::Failed(reason);
    }
    if !has_pending {
        let template = match overridden
            .as_ref()
            .map(|(template, _)| *template)
            .or_else(|| read_server_acix_template(server_id))
        {
            Some(template) => template,
            None => {
                return Outcome::Skipped(
                    "this server has no AnimeciX fansub. Set one with `/edit animecix_fansub:`"
                        .to_string(),
                )
            }
        };
        let drive = match link_value(uploaded, "drive") {
            Some(drive) => drive,
            None => {
                return Outcome::Skipped(
                    "job has no Google Drive link for AnimeciX multishare".to_string(),
                )
            }
        };
        let credits = AcixCredits {
            tl: credit(&meta.tl),
            tlc: credit(&meta.tlc),
            ts: credit(&meta.ts),
            qc: credit(&meta.qc),
        };
        let publish = AcixPublish {
            name: name.to_string(),
            mal_id,
            season_num: Some(season),
            episode_num: Some(episode),
            template,
            extra: credits.extra(),
            credits: Some(credits),
            acix_id,
        };
        let pending = AcixPending::new(publish, drive);
        let json = match serde_json::to_string(&pending) {
            Ok(json) => json,
            Err(e) => return Outcome::Failed(format!("could not queue the publish ({})", e)),
        };
        if let Err(e) = db.set_acix_pending(job_id, &json).await {
            return Outcome::Failed(format!("could not queue the publish ({})", e));
        }
    }

    let overrides = match CreditOverrides::from_values(extra, None, None, None, None) {
        Ok(overrides) => overrides,
        Err(e) => return Outcome::Failed(e),
    };
    // A record queued at upload time carries the fansub it was queued with, so an override has to
    // rewrite it — the record built just above already holds the overridden template.
    let overrides = match (&overridden, has_pending) {
        (Some((template, _)), true) => overrides.with_template(*template),
        _ => overrides,
    };
    match confirm_acix_with_overrides(db, job_id, overrides).await {
        Ok(value) => {
            let detail = match &overridden {
                Some((_, display)) => format!("multishare, multiple as {}", display),
                None => "multishare, multiple".to_string(),
            };
            if value.get("status").and_then(|status| status.as_str()) == Some("partial") {
                Outcome::Partial(detail)
            } else {
                Outcome::Published(detail)
            }
        }
        Err(e) if e.contains("already published") => Outcome::Skipped(e),
        Err(e) => Outcome::Failed(e),
    }
}

// An anime picked from the search arrives already resolved, because that search was OpenAnime's own.
// Anything recorded by a job or a channel still has to come back through the MyAnimeList id, which
// OpenAnime answers only by title alias and accepts only when the entry reports the same id. The
// season/episode must already exist there either way — this command creates nothing on OpenAnime.
#[allow(clippy::too_many_arguments)]
async fn publish_openanime(
    server_id: u64,
    uploaded: &serde_json::Value,
    meta: &ChannelMeta,
    credits: &CreditOverride,
    mal_id: i64,
    name: &str,
    resolved: Option<&Anime>,
    season: i64,
    episode: i64,
    fansub: SiteFansub<'_>,
) -> Outcome {
    // A named fansub was already resolved against the directory; a stored secure name is re-checked
    // here, because publishing under a name that no longer exists silently creates an orphan source
    // on OpenAnime.
    let fansub = match fansub {
        SiteFansub::Excluded => return excluded_outcome(FansubSite::OpenAnime),
        SiteFansub::Overridden(option) => option.clone(),
        SiteFansub::ServerDefault => {
            let secure_name = match read_server_fansub(server_id, FansubSite::OpenAnime) {
                Some(secure_name) => secure_name,
                None => {
                    return Outcome::Skipped(
                        "this server has no OpenAnime fansub. Set one with `/edit openanime_fansub:`"
                            .to_string(),
                    )
                }
            };
            match resolve_fansub_selection(FansubSite::OpenAnime, &secure_name).await {
                Ok(fansub) => fansub,
                Err(e) => return Outcome::Failed(e),
            }
        }
    };
    let (season, episode) = match (u32::try_from(season), u32::try_from(episode)) {
        (Ok(season), Ok(episode)) => (season, episode),
        _ => return Outcome::Failed("season/episode is out of range".to_string()),
    };
    let client = match OpenAnime::from_env() {
        Ok(client) => client,
        Err(e) => return Outcome::Failed(format!("client error: {}", e)),
    };
    let slug = match resolved {
        Some(anime) => anime.slug.clone(),
        None => match client.resolve_mal_id(mal_id as u64, name).await {
            Ok(anime) => anime.slug,
            Err(e) => return Outcome::Failed(format!("no entry for MAL id {}: {}", mal_id, e)),
        },
    };
    if let Err(e) = client.episode(&slug, season, episode).await {
        return Outcome::Failed(format!(
            "`{}` has no season {} episode {}: {}",
            slug, season, episode, e
        ));
    }
    let (players, skipped) = match plan_players(uploaded, Resolutions::new(true, false, false)) {
        Ok(planned) => planned,
        Err(e) => return Outcome::Skipped(e.trim_start_matches("Error: ").to_string()),
    };
    let contributors = credits.resolve(channel_credits(meta));

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
            .publish_episode(&slug, season, episode, &source)
            .await
        {
            Ok(_) => published.push(label),
            Err(e) => failed.push(format!("{}: {}", label, e)),
        }
    }
    let notes = if skipped.is_empty() {
        Vec::new()
    } else {
        vec![format!("skipped {}", skipped.join(", "))]
    };
    site_outcome(
        &format!("`{}` as {}", slug, fansub.display()),
        &published,
        &notes,
        &failed,
    )
}

// Anizm's staff panel exposes no MyAnimeList id, so the anime is matched by title and the site is
// skipped whenever that match is not unique — `/anizmconfirm` remains the way to name the id by
// hand. The episode must already be listed; only the fansub's translation relation is created.
async fn publish_anizm(
    server_id: u64,
    uploaded: &serde_json::Value,
    meta: &ChannelMeta,
    credits: &CreditOverride,
    name: &str,
    episode: i64,
    fansub: SiteFansub<'_>,
) -> Outcome {
    let overridden = matches!(fansub, SiteFansub::Overridden(_));
    let fansub_id = match fansub {
        SiteFansub::Excluded => return excluded_outcome(FansubSite::Anizm),
        SiteFansub::Overridden(option) => {
            match option.value.trim().parse::<u64>().ok().filter(|id| *id > 0) {
                Some(fansub_id) => fansub_id,
                None => {
                    return Outcome::Failed(format!(
                        "`{}` is not an Anizm fansub id",
                        option.value
                    ))
                }
            }
        }
        SiteFansub::ServerDefault => {
            match read_server_fansub(server_id, FansubSite::Anizm)
                .and_then(|value| value.trim().parse::<u64>().ok())
                .filter(|id| *id > 0)
            {
                Some(fansub_id) => fansub_id,
                None => {
                    return Outcome::Skipped(
                        "this server has no Anizm fansub. Set one with `/edit anizm_fansub:`"
                            .to_string(),
                    )
                }
            }
        }
    };
    let embeds = job_embed_links(uploaded);
    if embeds.is_empty() {
        return Outcome::Skipped(
            "job has no public streaming links; Anizm players are website embeds".to_string(),
        );
    }
    let catalog = match fetch_publishing_catalog().await {
        Ok(catalog) => catalog,
        Err(e) => return Outcome::Failed(format!("staff panel unavailable: {}", e)),
    };
    let anime = match match_anime_by_title(&catalog.anime, name) {
        Ok(anime) => anime,
        Err(e) => return Outcome::Skipped(e),
    };
    let fansub = match find_option(&catalog.fansubs, fansub_id) {
        Some(fansub) => fansub,
        None => {
            return Outcome::Failed(format!(
                "fansub id `{}` is no longer offered by this account. {}",
                fansub_id,
                if overridden {
                    "Pick another one for `anizm_fansub:`"
                } else {
                    "Reselect it with `/edit anizm_fansub:`"
                }
            ))
        }
    };
    let client = match Anizm::from_env() {
        Ok(client) => client,
        Err(e) => return Outcome::Failed(format!("client error: {}", e)),
    };
    let episode = episode as f64;
    let episodes = match client.episodes(anime.id).await {
        Ok(episodes) => episodes,
        Err(e) => return Outcome::Failed(e),
    };
    let episode_option = match find_episode_option(&episodes, episode) {
        Ok(Some(option)) => option,
        Ok(None) => {
            return Outcome::Skipped(format!(
                "{} Create it with `/anizmconfirm create_episode:true`.",
                episode_not_listed(&episodes, episode)
            ))
        }
        Err(e) => return Outcome::Failed(e),
    };

    // Anizm's translation relation is always named, so a cleared credit line falls back to the
    // fansub rather than creating a nameless relation.
    let translator = credits
        .resolve(credit(&meta.tl))
        .unwrap_or_else(|| fansub.label.clone());
    let mut notes = Vec::new();
    let translation = match resolve_or_create_translation(
        &client,
        anime.id,
        episode_option.id,
        &fansub,
        &TranslationCreate {
            anime_id: anime.id,
            episode_id: episode_option.id,
            translator,
            fansub_id,
            encoder: ANIZM_ENCODER.to_string(),
            bluray: false,
        },
        &mut notes,
    )
    .await
    {
        Ok(translation) => translation,
        Err(e) => return Outcome::Failed(e),
    };

    let mut published = Vec::new();
    let mut failed = Vec::new();
    for (label, embed) in embeds {
        let request = VideoCreate {
            anime_id: anime.id,
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
    site_outcome(
        &format!(
            "`{}` (#{}) episode `{}` as {}",
            anime.label, anime.id, episode_option.label, fansub.label
        ),
        &published,
        &notes,
        &failed,
    )
}

// Anizm's option labels are the only thing shared with the MyAnimeList-keyed sites, so a title that
// does not land on exactly one entry is reported instead of being resolved to a best guess.
fn match_anime_by_title(options: &[SelectOption], name: &str) -> Result<SelectOption, String> {
    let needle = normalize_title(name);
    if needle.is_empty() {
        return Err("the resolved anime has no title to match on Anizm".to_string());
    }
    let exact = options
        .iter()
        .filter(|option| normalize_title(&option.label) == needle)
        .collect::<Vec<_>>();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }
    if exact.is_empty() {
        let loose = options
            .iter()
            .filter(|option| normalize_title(&option.label).contains(&needle))
            .collect::<Vec<_>>();
        if loose.len() == 1 {
            return Ok(loose[0].clone());
        }
        if loose.is_empty() {
            return Err(format!(
                "no staff-panel anime matches `{}`. Publish it with `/anizmconfirm`",
                name
            ));
        }
        return Err(format!(
            "`{}` matches {} staff-panel entries. Publish it with `/anizmconfirm`",
            name,
            loose.len()
        ));
    }
    Err(format!(
        "`{}` matches {} staff-panel entries. Publish it with `/anizmconfirm`",
        name,
        exact.len()
    ))
}

fn normalize_title(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn site_outcome(
    target: &str,
    published: &[String],
    notes: &[String],
    failed: &[String],
) -> Outcome {
    let mut detail = vec![target.to_string()];
    if !published.is_empty() {
        detail.push(format!("players {}", published.join(", ")));
    }
    if !notes.is_empty() {
        detail.push(notes.join(", "));
    }
    if !failed.is_empty() {
        detail.push(format!("failed {}", failed.join("; ")));
    }
    let detail = detail.join(" — ");
    if failed.is_empty() {
        Outcome::Published(detail)
    } else if published.is_empty() {
        Outcome::Failed(detail)
    } else {
        Outcome::Partial(detail)
    }
}

async fn publish_response(
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

    fn option(id: u64, label: &str) -> SelectOption {
        SelectOption {
            id,
            label: label.to_string(),
        }
    }

    #[test]
    fn anime_choices_round_trip_through_the_submitted_value() {
        let value = anime_choice_value("Naruto", "naruto");
        let selection = parse_anime_option(&value).unwrap();
        assert_eq!(selection.slug, "naruto");
        assert_eq!(selection.title, "Naruto");
    }

    #[test]
    fn anime_choice_label_and_value_fit_the_discord_limit_and_keep_the_slug() {
        let long = "x".repeat(200);
        let label = anime_choice_label(&long, "some-long-slug");
        assert!(label.chars().count() <= MAX_CHOICE_CHARS);
        assert!(label.ends_with("(some-long-slug)"));

        // The title is what gets cut: the slug is the handle the entry is fetched by.
        let value = anime_choice_value(&long, "some-long-slug");
        assert!(value.chars().count() <= MAX_CHOICE_CHARS);
        assert_eq!(parse_anime_option(&value).unwrap().slug, "some-long-slug");
    }

    #[test]
    fn hand_typed_anime_titles_are_refused() {
        for value in ["Naruto", "20", "|Naruto", "naruto|", LOOKUP_FAILED_VALUE] {
            assert!(parse_anime_option(value).is_err(), "{}", value);
        }
    }

    #[test]
    fn the_credit_override_replaces_clears_or_keeps_every_sites_fallback() {
        let channel = || Some("Translator & Editor".to_string());

        assert_eq!(
            CreditOverride::parse(None).resolve(channel()),
            Some("Translator & Editor".to_string())
        );
        assert_eq!(
            CreditOverride::parse(Some("  Someone Else  ")).resolve(channel()),
            Some("Someone Else".to_string())
        );
        assert_eq!(CreditOverride::parse(Some("-")).resolve(channel()), None);
        // A site with nothing to fall back on stays empty rather than inventing a credit.
        assert_eq!(CreditOverride::parse(None).resolve(None), None);
    }

    #[test]
    fn lookup_failures_are_reported_inside_the_label_limit() {
        let short = lookup_failed_label("HTTP request failed.");
        assert_eq!(short, "⚠ OpenAnime lookup failed: HTTP request failed.");

        let long = lookup_failed_label(&"detail ".repeat(60));
        assert!(long.chars().count() <= MAX_CHOICE_CHARS, "{}", long);
        assert!(long.ends_with('…'), "{}", long);
    }

    #[test]
    fn anizm_titles_resolve_only_when_the_match_is_unique() {
        let options = vec![
            option(187, "Naruto"),
            option(17733, "Naruto: Akaki Yotsuba"),
            option(20, "Bleach"),
        ];
        // An exact label wins even though the same text is a prefix of another entry.
        assert_eq!(match_anime_by_title(&options, " naruto ").unwrap().id, 187);
        assert_eq!(match_anime_by_title(&options, "Bleach").unwrap().id, 20);
        // Only a containment match, and only one of them.
        assert_eq!(match_anime_by_title(&options, "Akaki").unwrap().id, 17733);

        let error = match_anime_by_title(&options, "One Piece").unwrap_err();
        assert!(error.contains("no staff-panel anime matches"), "{}", error);

        let ambiguous = vec![option(1, "Naruto"), option(2, "naruto")];
        let error = match_anime_by_title(&ambiguous, "Naruto").unwrap_err();
        assert!(error.contains("matches 2 staff-panel entries"), "{}", error);
    }

    #[test]
    fn a_site_that_publishes_some_of_its_links_is_partial_not_published() {
        let published = ["drive".to_string()];
        let failed = ["lulustream: HTTP 500".to_string()];
        let text = site_outcome("`slug` as Akira Subs", &published, &[], &failed).render("OpenAnime");
        assert!(text.starts_with("**OpenAnime** — Partially published:"), "{}", text);
        assert!(text.contains("players drive"), "{}", text);
        assert!(text.contains("failed lulustream: HTTP 500"), "{}", text);

        let text = site_outcome("`slug` as Akira Subs", &published, &[], &[]).render("OpenAnime");
        assert!(text.starts_with("**OpenAnime** — Published:"), "{}", text);

        let text = site_outcome("`slug` as Akira Subs", &[], &[], &failed).render("OpenAnime");
        assert!(text.starts_with("**OpenAnime** — Failed:"), "{}", text);
    }

    fn fansub(value: &str, name: &str) -> FansubOption {
        FansubOption::new(value.to_string(), name.to_string(), None, &[])
    }

    #[test]
    fn without_an_override_every_site_uses_the_server_selection() {
        let overrides = FansubOverrides::default();
        assert!(!overrides.is_active());
        for site in FansubSite::ALL {
            assert!(matches!(overrides.site(site), SiteFansub::ServerDefault), "{:?}", site);
        }
    }

    #[test]
    fn naming_one_site_excludes_the_ones_that_were_not_named() {
        let overrides = FansubOverrides {
            openanime: Some(fansub("akira-subs", "Akira Subs")),
            ..FansubOverrides::default()
        };
        assert!(overrides.is_active());
        match overrides.site(FansubSite::OpenAnime) {
            SiteFansub::Overridden(option) => assert_eq!(option.value, "akira-subs"),
            _ => panic!("the named site publishes under the named fansub"),
        }
        assert!(matches!(overrides.site(FansubSite::AnimeciX), SiteFansub::Excluded));
        assert!(matches!(overrides.site(FansubSite::Anizm), SiteFansub::Excluded));
    }

    #[test]
    fn naming_two_sites_leaves_only_the_third_blank() {
        let overrides = FansubOverrides {
            animecix: Some(fansub("218", "Akira Fansub")),
            openanime: Some(fansub("akira-subs", "Akira Subs")),
            anizm: None,
        };
        assert!(matches!(overrides.site(FansubSite::AnimeciX), SiteFansub::Overridden(_)));
        assert!(matches!(overrides.site(FansubSite::OpenAnime), SiteFansub::Overridden(_)));
        assert!(matches!(overrides.site(FansubSite::Anizm), SiteFansub::Excluded));

        let text = excluded_outcome(FansubSite::Anizm).render("Anizm");
        assert!(text.starts_with("**Anizm** — Skipped:"), "{}", text);
        assert!(text.contains("`anizm_fansub:`"), "{}", text);
    }

    #[test]
    fn skipped_sites_report_why_without_a_target() {
        let text = Outcome::Skipped(
            "this server has no Anizm fansub. Set one with `/edit anizm_fansub:`".to_string(),
        )
        .render("Anizm");
        assert_eq!(
            text,
            "**Anizm** — Skipped: this server has no Anizm fansub. Set one with `/edit anizm_fansub:`"
        );
    }
}
