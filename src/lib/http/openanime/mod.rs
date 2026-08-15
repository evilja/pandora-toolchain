pub mod core;

pub use core::{
    anime_titles, fetch_fansubs, refresh_fansubs, search_title, Anime, AnimeSearchResult,
    EpisodeSource, FansubChoice, OpenAnime, OpenAnimeError, Player, PlayerProvider, Resolutions,
};
