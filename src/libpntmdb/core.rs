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

pub enum TmdbKind {
    Movie { id: u64 },
    Series { id: u64 },
    Season { series_id: u64, season_number: u32 },
}

pub struct AnimeMeta {
    pub tmdb_id: u64,
    pub kind: AnimeKind,
    pub name: String,
    pub slug: String,
    pub episode_count: u32,
    pub season: Option<u32>,
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

pub fn parse_tmdb_url(url: &str) -> Result<TmdbKind, String> {
    let re_movie  = regex::Regex::new(r"^https?://(?:www\.)?themoviedb\.org/movie/(\d+)").unwrap();
    let re_season = regex::Regex::new(r"^https?://(?:www\.)?themoviedb\.org/tv/(\d+)/season/(\d+)").unwrap();
    let re_tv     = regex::Regex::new(r"^https?://(?:www\.)?themoviedb\.org/tv/(\d+)(?:$|/)").unwrap();

    if let Some(c) = re_season.captures(url) {
        let id = c.get(1).unwrap().as_str().parse::<u64>().map_err(|e| e.to_string())?;
        let n  = c.get(2).unwrap().as_str().parse::<u32>().map_err(|e| e.to_string())?;
        return Ok(TmdbKind::Season { series_id: id, season_number: n });
    }
    if let Some(c) = re_movie.captures(url) {
        let id = c.get(1).unwrap().as_str().parse::<u64>().map_err(|e| e.to_string())?;
        return Ok(TmdbKind::Movie { id });
    }
    if let Some(c) = re_tv.captures(url) {
        let id = c.get(1).unwrap().as_str().parse::<u64>().map_err(|e| e.to_string())?;
        return Ok(TmdbKind::Series { id });
    }
    Err(format!("URL is not a recognized TMDB movie/tv/season link: {}", url))
}

async fn fetch_json(client: &Client, api_key: &str, resource: &str) -> Result<Value, String> {
    let url = format!("https://api.themoviedb.org/3/{}?api_key={}", resource, api_key);
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("TMDB {} returned {}", resource, resp.status()));
    }
    resp.json::<Value>().await.map_err(|e| e.to_string())
}

pub async fn fetch_anime(url: &str) -> Result<AnimeMeta, String> {
    let kind = parse_tmdb_url(url)?;

    let env = get_env("env.pandora");
    if env.len() <= TMDB_API_KEY || env[TMDB_API_KEY].is_empty() {
        return Err("TMDB_API_KEY is not set in env.pandora".to_string());
    }
    let api_key = env[TMDB_API_KEY].clone();

    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;

    match kind {
        TmdbKind::Movie { id } => {
            let json = fetch_json(&client, &api_key, &format!("movie/{}", id)).await?;
            let name = json.get("title").and_then(|v| v.as_str())
                .ok_or_else(|| "TMDB response missing title".to_string())?
                .to_string();
            Ok(AnimeMeta {
                tmdb_id: id,
                kind: AnimeKind::Movie,
                name: name.clone(),
                slug: slugify(&name),
                episode_count: 1,
                season: None,
            })
        }
        TmdbKind::Series { id } => {
            let json = fetch_json(&client, &api_key, &format!("tv/{}", id)).await?;
            let name = json.get("name").and_then(|v| v.as_str())
                .ok_or_else(|| "TMDB response missing name".to_string())?
                .to_string();
            let n = json.get("number_of_episodes").and_then(|v| v.as_u64()).unwrap_or(0);
            if n == 0 {
                return Err("TMDB TV series has 0 episodes".to_string());
            }
            Ok(AnimeMeta {
                tmdb_id: id,
                kind: AnimeKind::MultiEpisode,
                name: name.clone(),
                slug: slugify(&name),
                episode_count: n as u32,
                season: None,
            })
        }
        TmdbKind::Season { series_id, season_number } => {
            let json = fetch_json(&client, &api_key, &format!("tv/{}", series_id)).await?;
            let series_name = json.get("name").and_then(|v| v.as_str())
                .ok_or_else(|| "TMDB response missing name".to_string())?
                .to_string();
            let seasons = json.get("seasons").and_then(|v| v.as_array())
                .ok_or_else(|| "TMDB response missing seasons".to_string())?;
            let s = seasons.iter()
                .find(|s| s.get("season_number").and_then(|v| v.as_u64()) == Some(season_number as u64))
                .ok_or_else(|| format!("Season {} not found for series {}", season_number, series_id))?;
            let ep_count = s.get("episode_count").and_then(|v| v.as_u64()).unwrap_or(0);
            if ep_count == 0 {
                return Err(format!("Season {} has 0 episodes", season_number));
            }
            let season_name = s.get("name").and_then(|v| v.as_str())
                .unwrap_or("Season")
                .to_string();
            let display = format!("{} {}", series_name, season_name);
            let series_slug = slugify(&series_name);
            Ok(AnimeMeta {
                tmdb_id: series_id,
                kind: AnimeKind::MultiEpisode,
                name: display,
                slug: format!("{}-s{}", series_slug, season_number),
                episode_count: ep_count as u32,
                season: Some(season_number),
            })
        }
    }
}
