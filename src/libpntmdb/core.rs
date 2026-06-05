use crate::libpnenv::{
    core::get_env,
    standard::TMDB_API_KEY,
};
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum AnimeKind {
    Movie,
    MultiEpisode,
}

pub struct AnimeMeta {
    pub tmdb_id: u64,
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

pub fn parse_tmdb_url(url: &str) -> Result<(String, u64), String> {
    let re_movie = regex::Regex::new(r"^https?://(?:www\.)?themoviedb\.org/movie/(\d+)").unwrap();
    let re_tv    = regex::Regex::new(r"^https?://(?:www\.)?themoviedb\.org/tv/(\d+)").unwrap();

    if let Some(c) = re_movie.captures(url) {
        let id = c.get(1).unwrap().as_str().parse::<u64>().map_err(|e| e.to_string())?;
        return Ok(("movie".to_string(), id));
    }
    if let Some(c) = re_tv.captures(url) {
        let id = c.get(1).unwrap().as_str().parse::<u64>().map_err(|e| e.to_string())?;
        return Ok(("tv".to_string(), id));
    }
    Err(format!("URL is not a recognized TMDB movie/tv link: {}", url))
}

pub async fn fetch_anime(url: &str) -> Result<AnimeMeta, String> {
    let (kind_str, id) = parse_tmdb_url(url)?;

    let env = get_env("env.pandora");
    if env.len() <= TMDB_API_KEY || env[TMDB_API_KEY].is_empty() {
        return Err("TMDB_API_KEY is not set in env.pandora".to_string());
    }
    let api_key = env[TMDB_API_KEY].clone();

    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;

    let api_url = format!("https://api.themoviedb.org/3/{}/{}?api_key={}", kind_str, id, api_key);
    let resp = client.get(&api_url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("TMDB API returned {}", resp.status()));
    }
    let json: Value = resp.json().await.map_err(|e| e.to_string())?;

    let name = json.get(if kind_str == "movie" { "title" } else { "name" })
        .and_then(|v| v.as_str())
        .ok_or_else(|| "TMDB response missing title/name".to_string())?
        .to_string();

    let (kind, episode_count) = if kind_str == "movie" {
        (AnimeKind::Movie, 1u32)
    } else {
        let n = json.get("number_of_episodes")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| "TV response missing number_of_episodes".to_string())?;
        if n == 0 {
            return Err("TMDB TV series has 0 episodes".to_string());
        }
        (AnimeKind::MultiEpisode, n as u32)
    };

    let slug = slugify(&name);

    Ok(AnimeMeta {
        tmdb_id: id,
        kind,
        name,
        slug,
        episode_count,
    })
}
