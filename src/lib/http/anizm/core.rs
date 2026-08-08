use capella::anizm::AnizmClient;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{ANIZM_EMAIL, ANIZM_PASSWORD};

pub use capella::anizm::{
    EpisodeCreate, Error as AnizmError, MutationResponse, PublishingCatalog, SelectOption,
    TranslationCreate, VideoCreate,
};

const CATALOG_CACHE_TTL_SECS: u64 = 5 * 60;

static CATALOG_CACHE: Mutex<Option<(Instant, PublishingCatalog)>> = Mutex::const_new(None);

// Anizm has no versioned API: the staff pages are the only authoritative source for which anime and
// fansub ids the account may publish under. The catalog is fetched through an authenticated page
// load, so it is cached briefly for the per-keystroke Discord autocomplete.
pub async fn fetch_publishing_catalog() -> Result<PublishingCatalog, String> {
    let mut cache = CATALOG_CACHE.lock().await;
    if let Some((fetched_at, catalog)) = cache.as_ref() {
        if fetched_at.elapsed() < Duration::from_secs(CATALOG_CACHE_TTL_SECS) {
            return Ok(catalog.clone());
        }
    }

    let catalog = Anizm::from_env()?.publishing_catalog().await?;
    if catalog.anime.is_empty() && catalog.fansubs.is_empty() {
        return Err("Anizm staff panel returned no anime or fansub options".to_string());
    }
    *cache = Some((Instant::now(), catalog.clone()));
    Ok(catalog)
}

pub struct Anizm {
    client: AnizmClient,
}

impl Anizm {
    pub fn with_credentials(email: String, password: String) -> Result<Self, String> {
        if email.is_empty() {
            return Err(format!(
                "Anizm email is empty. Set `{}` in env.pandora.",
                ANIZM_EMAIL
            ));
        }
        if password.is_empty() {
            return Err(format!(
                "Anizm password is empty. Set `{}` in env.pandora.",
                ANIZM_PASSWORD
            ));
        }
        let client = AnizmClient::with_credentials(email, password).map_err(stringify)?;
        Ok(Self { client })
    }

    pub fn from_env() -> Result<Self, String> {
        let env = get_pandora_env();
        Self::with_credentials(
            env.get(ANIZM_EMAIL).cloned().unwrap_or_default(),
            env.get(ANIZM_PASSWORD).cloned().unwrap_or_default(),
        )
    }

    pub async fn publishing_catalog(&self) -> Result<PublishingCatalog, String> {
        self.client.publishing_catalog().await.map_err(stringify)
    }

    pub async fn episodes(&self, anime_id: u64) -> Result<Vec<SelectOption>, String> {
        self.client
            .episodes_for_anime(anime_id)
            .await
            .map_err(stringify)
    }

    pub async fn translation_relations(
        &self,
        anime_id: u64,
        episode_id: u64,
    ) -> Result<Vec<SelectOption>, String> {
        self.client
            .translation_relations(anime_id, episode_id)
            .await
            .map_err(stringify)
    }

    pub async fn add_episode(&self, request: &EpisodeCreate) -> Result<MutationResponse, String> {
        self.client.add_episode(request).await.map_err(stringify)
    }

    pub async fn add_translation(
        &self,
        request: &TranslationCreate,
    ) -> Result<MutationResponse, String> {
        self.client
            .add_translation(request)
            .await
            .map_err(stringify)
    }

    pub async fn add_video(&self, request: &VideoCreate) -> Result<MutationResponse, String> {
        self.client.add_video(request).await.map_err(stringify)
    }
}

// Anizm episode options are labelled in Turkish (`12. Bölüm`, `219. Bölüm (Filler)`), so the
// episode number is read from the leading numeric token. Options that do not start with a number
// (specials, movies) simply never match a requested number instead of being guessed at.
pub fn episode_number_from_label(label: &str) -> Option<f64> {
    let mut number = String::new();
    for character in label.trim().chars() {
        match character {
            '0'..='9' => number.push(character),
            '.' | ',' if !number.is_empty() && !number.contains('.') => number.push('.'),
            _ => break,
        }
    }
    let number = number.trim_end_matches('.');
    if number.is_empty() {
        return None;
    }
    number.parse::<f64>().ok()
}

// Resolution is by parsed episode number against the ids the staff form actually offers. `Ok(None)`
// means the number is simply not listed; several matching options are an error rather than a pick,
// because publishing to the wrong episode id cannot be undone from Pandora.
pub fn find_episode_option(
    episodes: &[SelectOption],
    episode: f64,
) -> Result<Option<SelectOption>, String> {
    let matches = episodes
        .iter()
        .filter(|option| {
            episode_number_from_label(&option.label)
                .is_some_and(|number| (number - episode).abs() < 0.000_001)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [single] => Ok(Some((*single).clone())),
        [] => Ok(None),
        multiple => Err(format!(
            "Anizm lists {} options for episode `{}`: {}. Resolve the duplicate on Anizm first.",
            multiple.len(),
            format_episode(episode),
            multiple
                .iter()
                .map(|option| format!("{} (#{})", option.label, option.id))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

pub fn episode_not_listed(episodes: &[SelectOption], episode: f64) -> String {
    format!(
        "Anizm has no episode `{}` for this anime. Available: {}",
        format_episode(episode),
        summarize_options(episodes)
    )
}

// The translation relation id is what a video is attached to, and the staff form labels it with the
// fansub name. An exact name match wins; otherwise a single containing label is accepted and
// anything ambiguous is refused.
pub fn resolve_translation_option(
    relations: &[SelectOption],
    fansub_label: &str,
) -> Result<SelectOption, String> {
    let wanted = normalize(fansub_label);
    if wanted.is_empty() {
        return Err("Anizm fansub name is empty".to_string());
    }
    let exact = relations
        .iter()
        .filter(|option| normalize(&option.label) == wanted)
        .collect::<Vec<_>>();
    if let [single] = exact.as_slice() {
        return Ok((*single).clone());
    }
    if exact.len() > 1 {
        return Err(format!(
            "Anizm lists {} translation relations named `{}`; resolve the duplicate on Anizm first.",
            exact.len(),
            fansub_label
        ));
    }
    let partial = relations
        .iter()
        .filter(|option| normalize(&option.label).contains(&wanted))
        .collect::<Vec<_>>();
    match partial.as_slice() {
        [single] => Ok((*single).clone()),
        [] => Err(format!(
            "no Anizm translation relation for fansub `{}`. Existing relations: {}",
            fansub_label,
            summarize_options(relations)
        )),
        multiple => Err(format!(
            "fansub `{}` matches {} Anizm translation relations: {}",
            fansub_label,
            multiple.len(),
            multiple
                .iter()
                .map(|option| format!("{} (#{})", option.label, option.id))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

pub fn find_option(options: &[SelectOption], id: u64) -> Option<SelectOption> {
    options.iter().find(|option| option.id == id).cloned()
}

pub fn format_episode(episode: f64) -> String {
    if (episode.fract()).abs() < 0.000_001 {
        format!("{}", episode as i64)
    } else {
        format!("{}", episode)
    }
}

fn summarize_options(options: &[SelectOption]) -> String {
    if options.is_empty() {
        return "none".to_string();
    }
    let shown = options
        .iter()
        .take(10)
        .map(|option| option.label.clone())
        .collect::<Vec<_>>()
        .join(", ");
    if options.len() > 10 {
        format!("{}, …(+{})", shown, options.len() - 10)
    } else {
        shown
    }
}

fn normalize(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn stringify(error: AnizmError) -> String {
    error.to_string()
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
    fn reads_episode_numbers_from_turkish_labels() {
        assert_eq!(episode_number_from_label("12. Bölüm"), Some(12.0));
        assert_eq!(
            episode_number_from_label(" 219. Bölüm (Filler) "),
            Some(219.0)
        );
        assert_eq!(episode_number_from_label("220. Bölüm Final"), Some(220.0));
        assert_eq!(episode_number_from_label("7.5. Bölüm"), Some(7.5));
        assert_eq!(episode_number_from_label("Özel Bölüm"), None);
        assert_eq!(episode_number_from_label(""), None);
    }

    #[test]
    fn resolves_exactly_one_episode_option() {
        let episodes = vec![option(1, "1. Bölüm"), option(2, "2. Bölüm"), option(3, "Film")];
        assert_eq!(find_episode_option(&episodes, 2.0).unwrap().unwrap().id, 2);
        assert_eq!(find_episode_option(&episodes, 9.0).unwrap(), None);
        let missing = episode_not_listed(&episodes, 9.0);
        assert!(missing.contains("no episode `9`"), "{}", missing);
        assert!(missing.contains("1. Bölüm"), "{}", missing);
    }

    #[test]
    fn refuses_to_guess_between_duplicate_episode_options() {
        let episodes = vec![option(10, "5. Bölüm"), option(11, "5. Bölüm (Filler)")];
        let error = find_episode_option(&episodes, 5.0).unwrap_err();
        assert!(error.contains("2 options"), "{}", error);
        assert!(error.contains("#10") && error.contains("#11"), "{}", error);
    }

    #[test]
    fn matches_translation_relation_by_name_then_containment() {
        let relations = vec![option(70, "Akira Subs"), option(71, "Other Fansub")];
        assert_eq!(
            resolve_translation_option(&relations, "  akira   subs ")
                .unwrap()
                .id,
            70
        );

        let partial = vec![option(80, "Akira Subs & Friends"), option(81, "Other")];
        assert_eq!(resolve_translation_option(&partial, "Akira Subs").unwrap().id, 80);
    }

    #[test]
    fn ambiguous_or_missing_relations_are_errors() {
        let ambiguous = vec![option(80, "Akira Subs A"), option(81, "Akira Subs B")];
        assert!(resolve_translation_option(&ambiguous, "Akira Subs").is_err());
        assert!(resolve_translation_option(&[], "Akira Subs").is_err());
    }

    #[test]
    fn option_lookup_matches_on_id_only() {
        let options = vec![option(5, "Anime A"), option(6, "Anime B")];
        assert_eq!(find_option(&options, 6).unwrap().label, "Anime B");
        assert!(find_option(&options, 7).is_none());
    }
}
