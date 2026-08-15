use capella::openanime::{Fansub, OpenAnimeClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{OPENANIME_EMAIL, OPENANIME_PASSWORD};
use crate::lib::http::directory::{self, MemoryCache};

pub use capella::openanime::{
    Anime, AnimeSearchResult, EpisodeSource, Error as OpenAnimeError, Player, PlayerProvider,
    Resolutions,
};

const DIRECTORY_SITE: &str = "openanime";

static FANSUB_CACHE: MemoryCache<Vec<FansubChoice>> = Mutex::const_new(None);

// OpenAnime episode sources are addressed by `fansubSecureName`, not by the display name, so a
// fansub without a secure name is not selectable and is dropped while building the directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FansubChoice {
    pub secure_name: String,
    pub name: String,
}

impl FansubChoice {
    pub fn display_name(&self) -> String {
        if self.name.eq_ignore_ascii_case(&self.secure_name) {
            self.name.clone()
        } else {
            format!("{} — {}", self.name, self.secure_name)
        }
    }
}

// Served from the persisted directory so a keystroke never waits on OpenAnime; see
// `lib::http::directory` for the refresh and staleness rules.
pub async fn fetch_fansubs() -> Result<Vec<FansubChoice>, String> {
    directory::cached(DIRECTORY_SITE, &FANSUB_CACHE, fetch_fansubs_uncached).await
}

pub async fn refresh_fansubs() -> Result<Vec<FansubChoice>, String> {
    directory::refresh_now(DIRECTORY_SITE, &FANSUB_CACHE, fetch_fansubs_uncached).await
}

// An empty directory is an error rather than a cached result, so a transient bad response cannot
// overwrite a good copy on disk with nothing.
async fn fetch_fansubs_uncached() -> Result<Vec<FansubChoice>, String> {
    let fansubs = OpenAnime::from_env()?.fansubs().await?;
    if fansubs.is_empty() {
        return Err("OpenAnime returned no fansubs with a secure name".to_string());
    }
    Ok(fansubs)
}

pub struct OpenAnime {
    client: OpenAnimeClient,
}

impl OpenAnime {
    pub fn with_credentials(email: String, password: String) -> Result<Self, String> {
        if email.is_empty() {
            return Err(format!(
                "OpenAnime email is empty. Set `{}` in env.pandora.",
                OPENANIME_EMAIL
            ));
        }
        if password.is_empty() {
            return Err(format!(
                "OpenAnime password is empty. Set `{}` in env.pandora.",
                OPENANIME_PASSWORD
            ));
        }
        let client = OpenAnimeClient::with_credentials(email, password).map_err(stringify)?;
        Ok(Self { client })
    }

    pub fn from_env() -> Result<Self, String> {
        let env = get_pandora_env();
        Self::with_credentials(
            env.get(OPENANIME_EMAIL).cloned().unwrap_or_default(),
            env.get(OPENANIME_PASSWORD).cloned().unwrap_or_default(),
        )
    }

    // Catalog reads are unauthenticated, so a search keystroke does not have to wait on — or fail
    // on — the publishing credentials. Only publishing needs `from_env`.
    pub fn catalog() -> Result<Self, String> {
        Ok(Self {
            client: OpenAnimeClient::new().map_err(stringify)?,
        })
    }

    pub async fn search(&self, query: &str) -> Result<Vec<AnimeSearchResult>, String> {
        self.client.search(query).await.map_err(stringify)
    }

    // Pandora publishes through the admin dashboard's episode form, which searches the whole
    // directory (`GET /fansub/all`) rather than the fansub panel's `GET /user/fansubs`, so every
    // fansub is selectable and not just the ones the account is a member of. Narrowing to the
    // account list on error is deliberately avoided: a partial directory would reject a valid
    // stored secure name as unknown instead of reporting that the lookup failed.
    pub async fn fansubs(&self) -> Result<Vec<FansubChoice>, String> {
        Ok(fansub_choices(
            self.client.public_fansubs().await.map_err(stringify)?,
        ))
    }

    // Capella resolves through title aliases but accepts a candidate only when its detail response
    // carries the exact requested `malID`, so an approximate title never publishes to the wrong
    // entry.
    pub async fn resolve_mal_id(&self, mal_id: u64, title: &str) -> Result<Anime, String> {
        self.client
            .resolve_mal_id_with_title(mal_id, title)
            .await
            .map_err(stringify)
    }

    pub async fn anime(&self, slug: &str) -> Result<Anime, String> {
        self.client.anime(slug).await.map_err(stringify)
    }

    pub async fn episode(&self, slug: &str, season: u32, episode: u32) -> Result<Value, String> {
        self.client
            .episode(slug, season, episode, None)
            .await
            .map_err(stringify)
    }

    pub async fn publish_episode(
        &self,
        slug: &str,
        season: u32,
        episode: u32,
        source: &EpisodeSource,
    ) -> Result<Option<Value>, String> {
        self.client
            .publish_episode(slug, season, episode, source)
            .await
            .map_err(stringify)
    }
}

// OpenAnime keeps one title per language and none of them is authoritative, so every alias is
// offered rather than one guess. Romaji leads because the catalogs that have to be searched by title
// are built from TMDB, and the native script trails because a search for it rarely matches there.
pub fn anime_titles(anime: &Anime) -> Vec<String> {
    let mut titles: Vec<String> = Vec::new();
    for title in [
        &anime.romaji,
        &anime.english,
        &anime.turkish,
        &anime.japanese,
    ] {
        let Some(title) = title.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
            continue;
        };
        if !titles.iter().any(|kept| kept.eq_ignore_ascii_case(title)) {
            titles.push(title.to_string());
        }
    }
    titles
}

// The TMDB id OpenAnime built the entry from, and whether it is a movie. Capella does not model
// `tmdbID`, so it is read off the retained raw response. This is the id that actually joins the two
// catalogs: both OpenAnime and AnimeciX import from TMDB, while their MyAnimeList ids disagree
// whenever AnimeciX files several seasons under one entry or records an unrelated id.
pub fn tmdb_reference(anime: &Anime) -> Option<(String, bool)> {
    let tmdb_id = anime.raw.get("tmdbID").and_then(|value| match value {
        Value::String(text) => Some(text.trim().to_string()),
        Value::Number(number) => number.as_i64().map(|id| id.to_string()),
        _ => None,
    })?;
    if tmdb_id.is_empty() || !tmdb_id.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let is_movie = anime
        .media_type
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("movie"));
    Some((tmdb_id, is_movie))
}

// The title a search result is offered under. Same preference as `anime_titles` minus the native
// script, which a search response does not carry anyway.
pub fn search_title(result: &AnimeSearchResult) -> Option<String> {
    [&result.romaji, &result.english, &result.turkish]
        .into_iter()
        .find_map(|title| {
            title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
        })
}

fn fansub_choices(fansubs: Vec<Fansub>) -> Vec<FansubChoice> {
    fansubs
        .into_iter()
        .filter_map(|fansub| {
            let secure_name = fansub.secure_name?.trim().to_string();
            if secure_name.is_empty() {
                return None;
            }
            Some(FansubChoice {
                secure_name,
                name: fansub.name,
            })
        })
        .collect()
}

fn stringify(error: OpenAnimeError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::{anime_titles, fansub_choices, search_title, tmdb_reference, FansubChoice};
    use capella::openanime::{Anime, AnimeSearchResult, Fansub};
    use serde_json::json;

    fn anime(romaji: Option<&str>, english: Option<&str>, turkish: Option<&str>, japanese: Option<&str>) -> Anime {
        Anime {
            id: None,
            slug: "slug".to_string(),
            mal_id: None,
            english: english.map(str::to_string),
            turkish: turkish.map(str::to_string),
            japanese: japanese.map(str::to_string),
            romaji: romaji.map(str::to_string),
            media_type: None,
            number_of_episodes: None,
            raw: json!({}),
        }
    }

    fn search_result(
        romaji: Option<&str>,
        english: Option<&str>,
        turkish: Option<&str>,
    ) -> AnimeSearchResult {
        AnimeSearchResult {
            id: None,
            slug: "slug".to_string(),
            english: english.map(str::to_string),
            turkish: turkish.map(str::to_string),
            romaji: romaji.map(str::to_string),
        }
    }

    #[test]
    fn every_alias_is_offered_once_in_search_order() {
        assert_eq!(
            anime_titles(&anime(
                Some("Shingeki no Kyojin"),
                Some("Attack on Titan"),
                Some("  "),
                Some("進撃の巨人"),
            )),
            vec!["Shingeki no Kyojin", "Attack on Titan", "進撃の巨人"]
        );
        // A catalog that repeats one title across languages must not make the same search twice.
        assert_eq!(
            anime_titles(&anime(Some("Naruto"), Some("naruto"), None, None)),
            vec!["Naruto"]
        );
        assert!(anime_titles(&anime(None, None, None, None)).is_empty());
    }

    #[test]
    fn the_tmdb_reference_reads_either_json_shape_and_refuses_anything_else() {
        let mut anime = anime(Some("Frieren"), None, None, None);
        anime.raw = json!({ "tmdbID": 209867 });
        assert_eq!(
            tmdb_reference(&anime),
            Some(("209867".to_string(), false))
        );

        // OpenAnime has returned the id as a string as well, and the import only accepts digits.
        anime.raw = json!({ "tmdbID": " 83121 " });
        assert_eq!(tmdb_reference(&anime), Some(("83121".to_string(), false)));
        anime.media_type = Some("movie".to_string());
        assert_eq!(tmdb_reference(&anime), Some(("83121".to_string(), true)));

        for raw in [json!({}), json!({ "tmdbID": "" }), json!({ "tmdbID": "tt0903747" })] {
            anime.raw = raw;
            assert_eq!(tmdb_reference(&anime), None);
        }
    }

    #[test]
    fn a_search_result_without_a_readable_title_is_not_offerable() {
        assert_eq!(
            search_title(&search_result(None, Some("Attack on Titan"), Some("Titan"))),
            Some("Attack on Titan".to_string())
        );
        assert_eq!(search_title(&search_result(Some("  "), None, None)), None);
    }

    fn fansub(name: &str, secure_name: Option<&str>) -> Fansub {
        Fansub {
            id: Some("id".to_string()),
            name: name.to_string(),
            secure_name: secure_name.map(str::to_string),
            avatar: None,
            banner: None,
            raw: json!({}),
        }
    }

    #[test]
    fn unpublishable_fansubs_without_a_secure_name_are_dropped() {
        let choices = fansub_choices(vec![
            fansub("Akira Subs", Some("akira-subs")),
            fansub("No Secure Name", None),
            fansub("Blank", Some("   ")),
        ]);
        assert_eq!(
            choices,
            vec![FansubChoice {
                secure_name: "akira-subs".to_string(),
                name: "Akira Subs".to_string(),
            }]
        );
    }

    #[test]
    fn display_name_keeps_the_secure_name_visible_when_it_differs() {
        assert_eq!(
            fansub_choices(vec![fansub("Akira Subs", Some("akira-subs"))])[0].display_name(),
            "Akira Subs — akira-subs"
        );
        assert_eq!(
            fansub_choices(vec![fansub("AkiraSubs", Some("akirasubs"))])[0].display_name(),
            "AkiraSubs"
        );
    }
}
