use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum AnimeKind {
    Movie,
    MultiEpisode,
}

pub struct AnimeMeta {
    pub mal_id: u64,
    pub kind: AnimeKind,
    pub name: String,
    pub slug: String,
    pub episode_count: u32,
    pub year: Option<u16>,
    pub season: u16,
}

pub fn slugify(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut prev_dash = true;
    for ch in lower.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "anime".to_string()
    } else {
        trimmed
    }
}

pub fn parse_mal_url(url: &str) -> Result<u64, String> {
    let re = regex::Regex::new(r"^https?://myanimelist\.net/anime/(\d+)(?:/[^/?#]*)?(?:[?#].*)?$").unwrap();
    let caps = re.captures(url)
        .ok_or_else(|| format!("URL is not a recognized MyAnimeList anime link: {}", url))?;
    let id = caps.get(1).unwrap().as_str().parse::<u64>().map_err(|e| e.to_string())?;
    Ok(id)
}

fn episode_count(data: &Value, source: &str, id: u64) -> Result<u32, String> {
    let episodes = data.get("episodes")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| format!("{} response missing or null `episodes` for anime {} (likely ongoing — episode count not yet announced)", source, id))?;
    if episodes == 0 {
        return Err(format!("{} reports 0 episodes for anime {}", source, id));
    }
    u32::try_from(episodes).map_err(|_| format!("{} returned an invalid episode count for anime {}", source, id))
}

fn meta_from_jikan(id: u64, body: &Value) -> Result<AnimeMeta, String> {
    let data = body.get("data")
        .ok_or_else(|| "JIKAN response missing `data`".to_string())?;

    let name = data.get("title_english").and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| data.get("title").and_then(|v| v.as_str()))
        .ok_or_else(|| "JIKAN response missing title/title_english".to_string())?
        .to_string();
    let episode_count = episode_count(data, "JIKAN", id)?;
    let kind = match data.get("type").and_then(|v| v.as_str()).unwrap_or("") {
        "Movie" => AnimeKind::Movie,
        _ => AnimeKind::MultiEpisode,
    };
    let year = data.get("year").and_then(|v| v.as_u64())
        .and_then(|y| u16::try_from(y).ok())
        .or_else(|| {
            data.get("aired")
                .and_then(|a| a.get("from"))
                .and_then(|f| f.as_str())
                .and_then(|s| if s.len() >= 4 { s.get(..4) } else { None })
                .and_then(|y| y.parse::<u16>().ok())
        });

    Ok(AnimeMeta {
        mal_id: id,
        kind,
        slug: slugify(&name),
        name,
        episode_count,
        year,
        season: 1,
    })
}

fn meta_from_anilist(id: u64, body: &Value) -> Result<AnimeMeta, String> {
    if let Some(errors) = body.get("errors") {
        return Err(format!("AniList returned GraphQL errors: {}", errors));
    }
    let data = body.get("data")
        .and_then(|v| v.get("Media"))
        .filter(|v| !v.is_null())
        .ok_or_else(|| format!("AniList has no anime with MAL id {}", id))?;
    let returned_id = data.get("idMal").and_then(|v| v.as_u64())
        .ok_or_else(|| "AniList response missing `idMal`".to_string())?;
    if returned_id != id {
        return Err(format!("AniList returned MAL id {} for anime {}", returned_id, id));
    }

    let titles = data.get("title").ok_or_else(|| "AniList response missing `title`".to_string())?;
    let name = titles.get("english").and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| titles.get("romaji").and_then(|v| v.as_str()))
        .ok_or_else(|| "AniList response missing title.english/title.romaji".to_string())?
        .to_string();
    let episode_count = episode_count(data, "AniList", id)?;
    let kind = match data.get("format").and_then(|v| v.as_str()).unwrap_or("") {
        "MOVIE" => AnimeKind::Movie,
        _ => AnimeKind::MultiEpisode,
    };
    let year = data.get("startDate")
        .and_then(|v| v.get("year"))
        .and_then(|v| v.as_u64())
        .and_then(|y| u16::try_from(y).ok());

    Ok(AnimeMeta {
        mal_id: id,
        kind,
        slug: slugify(&name),
        name,
        episode_count,
        year,
        season: 1,
    })
}

async fn fetch_jikan(client: &Client, id: u64) -> Result<AnimeMeta, String> {
    let api_url = format!("https://api.jikan.moe/v4/anime/{}", id);
    let resp = client.get(&api_url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("JIKAN returned {} for anime {}", resp.status(), id));
    }
    let body: Value = resp.json().await.map_err(|e| format!("JIKAN returned invalid JSON: {}", e))?;
    meta_from_jikan(id, &body)
}

async fn fetch_anilist(client: &Client, id: u64) -> Result<AnimeMeta, String> {
    let query = r#"query ($idMal: Int) {
        Media(idMal: $idMal, type: ANIME) {
            idMal
            episodes
            format
            title { english romaji }
            startDate { year }
        }
    }"#;
    let payload = serde_json::json!({
        "query": query,
        "variables": { "idMal": id },
    });
    let resp = client.post("https://graphql.anilist.co")
        .json(&payload)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("AniList returned {} for anime {}", resp.status(), id));
    }
    let body: Value = resp.json().await.map_err(|e| format!("AniList returned invalid JSON: {}", e))?;
    meta_from_anilist(id, &body)
}

// JIKAN occasionally returns a gateway error for valid MAL entries, so AniList is
// used as a structured fallback keyed by the same MyAnimeList id.
pub async fn fetch_anime(url: &str) -> Result<AnimeMeta, String> {
    let id = parse_mal_url(url)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;

    match fetch_jikan(&client, id).await {
        Ok(meta) => Ok(meta),
        Err(jikan_error) => fetch_anilist(&client, id).await
            .map_err(|anilist_error| format!("{}; fallback failed: {}", jikan_error, anilist_error)),
    }
}

// How many episodes MyAnimeList counts before this entry, following its prequel chain: a group
// numbering a franchise straight through starts this entry at that count plus one. Only TV and
// ONA entries count — a movie or an OVA between seasons takes no number in such a run. `None`
// whenever any link of the chain cannot be read, including a prequel whose count is unknown,
// because a partial sum is a confident wrong guess and no guess is only a question.
pub async fn prequel_episode_total(id: u64) -> Option<u32> {
    const MAX_HOPS: usize = 12;
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .ok()?;
    let mut total = 0u32;
    let mut current = id;
    let mut visited = vec![id];
    for _ in 0..MAX_HOPS {
        let Some(prequel) = jikan_prequel(&client, current).await? else {
            return Some(total);
        };
        if visited.contains(&prequel) {
            return Some(total);
        }
        visited.push(prequel);
        // JIKAN allows three requests a second; a chain walk is a burst of them.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let body: Value = jikan_get(&client, &format!("https://api.jikan.moe/v4/anime/{}", prequel)).await?;
        let data = body.get("data")?;
        let counts = matches!(data.get("type").and_then(|v| v.as_str()), Some("TV") | Some("ONA"));
        if counts {
            let episodes = data.get("episodes").and_then(|v| v.as_u64())?;
            total = total.checked_add(u32::try_from(episodes).ok()?)?;
        }
        current = prequel;
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    Some(total)
}

// The outer `None` is a failed request; the inner one an entry with no prequel.
async fn jikan_prequel(client: &Client, id: u64) -> Option<Option<u64>> {
    let body = jikan_get(client, &format!("https://api.jikan.moe/v4/anime/{}/relations", id)).await?;
    Some(prequel_from_relations(&body))
}

async fn jikan_get(client: &Client, url: &str) -> Option<Value> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

fn prequel_from_relations(body: &Value) -> Option<u64> {
    body.get("data")?
        .as_array()?
        .iter()
        .filter(|relation| relation.get("relation").and_then(|v| v.as_str()) == Some("Prequel"))
        .flat_map(|relation| relation.get("entry").and_then(|v| v.as_array()).cloned().unwrap_or_default())
        .find(|entry| entry.get("type").and_then(|v| v.as_str()) == Some("anime"))
        .and_then(|entry| entry.get("mal_id").and_then(|v| v.as_u64()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prequel_is_the_first_anime_entry_under_prequel() {
        let body = serde_json::json!({
            "data": [
                { "relation": "Adaptation", "entry": [{ "mal_id": 1, "type": "manga" }] },
                { "relation": "Prequel", "entry": [
                    { "mal_id": 2, "type": "manga" },
                    { "mal_id": 3, "type": "anime" }
                ] },
                { "relation": "Sequel", "entry": [{ "mal_id": 4, "type": "anime" }] }
            ]
        });
        assert_eq!(prequel_from_relations(&body), Some(3));
        assert_eq!(prequel_from_relations(&serde_json::json!({ "data": [] })), None);
    }

    #[test]
    fn parses_anilist_fallback_metadata() {
        let body = serde_json::json!({
            "data": {
                "Media": {
                    "idMal": 62155,
                    "episodes": 6,
                    "format": "ONA",
                    "title": {
                        "english": "I Want You To Show Me Your Panties With a Disgusted Face Returns",
                        "romaji": "Iya na Kao sare nagara Opantsu Misete Moraitai Returns"
                    },
                    "startDate": { "year": 2026 }
                }
            }
        });

        let meta = meta_from_anilist(62155, &body).unwrap();
        assert_eq!(meta.mal_id, 62155);
        assert_eq!(meta.episode_count, 6);
        assert_eq!(meta.year, Some(2026));
        assert_eq!(meta.slug, "i-want-you-to-show-me-your-panties-with-a-disgusted-face-returns");
        assert!(matches!(meta.kind, AnimeKind::MultiEpisode));
    }

    #[test]
    fn prefers_anilist_english_title_and_recognizes_movies() {
        let body = serde_json::json!({
            "data": {
                "Media": {
                    "idMal": 1,
                    "episodes": 1,
                    "format": "MOVIE",
                    "title": { "english": "English Name", "romaji": "Romaji Name" },
                    "startDate": { "year": 2001 }
                }
            }
        });

        let meta = meta_from_anilist(1, &body).unwrap();
        assert_eq!(meta.name, "English Name");
        assert!(matches!(meta.kind, AnimeKind::Movie));
    }
}
