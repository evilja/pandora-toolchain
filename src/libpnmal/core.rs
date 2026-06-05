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

pub async fn fetch_anime(url: &str) -> Result<AnimeMeta, String> {
    let id = parse_mal_url(url)?;

    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;

    let api_url = format!("https://api.jikan.moe/v4/anime/{}", id);
    let resp = client.get(&api_url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("JIKAN returned {} for anime {}", resp.status(), id));
    }
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    let data = body.get("data")
        .ok_or_else(|| "JIKAN response missing `data`".to_string())?;

    let name = data.get("title_english").and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| data.get("title").and_then(|v| v.as_str()))
        .ok_or_else(|| "JIKAN response missing title/title_english".to_string())?
        .to_string();

    let episodes = data.get("episodes")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| format!("JIKAN response missing or null `episodes` for anime {} (likely ongoing — episode count not yet announced)", id))?;
    if episodes == 0 {
        return Err(format!("JIKAN reports 0 episodes for anime {}", id));
    }

    let kind_str = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let kind = match kind_str {
        "Movie" => AnimeKind::Movie,
        _ => AnimeKind::MultiEpisode,
    };

    let slug = slugify(&name);

    Ok(AnimeMeta {
        mal_id: id,
        kind,
        name,
        slug,
        episode_count: episodes as u32,
    })
}
