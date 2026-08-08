use capella::openanime::{Fansub, OpenAnimeClient};
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{OPENANIME_EMAIL, OPENANIME_PASSWORD};

pub use capella::openanime::{
    Anime, EpisodeSource, Error as OpenAnimeError, Player, PlayerProvider, Resolutions,
};

const FANSUB_CACHE_TTL_SECS: u64 = 5 * 60;

static FANSUB_CACHE: Mutex<Option<(Instant, Vec<FansubChoice>)>> = Mutex::const_new(None);

// OpenAnime episode sources are addressed by `fansubSecureName`, not by the display name, so a
// fansub without a secure name is not selectable and is dropped while building the directory.
#[derive(Clone, Debug, PartialEq, Eq)]
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

// Discord requests autocomplete after nearly every keystroke, so the directory is cached briefly
// like the AnimeciX translator directory is.
pub async fn fetch_fansubs() -> Result<Vec<FansubChoice>, String> {
    let mut cache = FANSUB_CACHE.lock().await;
    if let Some((fetched_at, fansubs)) = cache.as_ref() {
        if fetched_at.elapsed() < Duration::from_secs(FANSUB_CACHE_TTL_SECS) {
            return Ok(fansubs.clone());
        }
    }

    let fansubs = OpenAnime::from_env()?.fansubs().await?;
    if fansubs.is_empty() {
        return Err("OpenAnime returned no fansubs with a secure name".to_string());
    }
    *cache = Some((Instant::now(), fansubs.clone()));
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
    use super::{fansub_choices, FansubChoice};
    use capella::openanime::Fansub;
    use serde_json::json;

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
