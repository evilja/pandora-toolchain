use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

// Fansub/anime autocomplete directories are large, slow to fetch, and change rarely. Discord
// discards an autocomplete response that takes longer than three seconds, and an authenticated
// staff-panel load does not reliably fit in that budget, so a directory is never fetched on the
// keystroke path if any copy exists: the on-disk copy under DB/cache/directories is returned
// immediately and a stale one is refreshed in the background. A failed refresh keeps the previous
// copy rather than emptying the selector, which is the difference between a slightly outdated list
// and no list at all.
pub const REFRESH_INTERVAL_SECS: u64 = 12 * 60 * 60;

const CACHE_DIR: &str = "DB/cache/directories";

// Only one background refresh per directory: Discord fires an autocomplete per keystroke, so a
// stale copy would otherwise start a login for every character typed.
static REFRESHING: StdMutex<Option<HashSet<&'static str>>> = StdMutex::new(None);

// Wall-clock rather than `Instant`, because the timestamp has to survive a restart on disk.
#[derive(Serialize, Deserialize)]
struct Stored<T> {
    fetched_at: u64,
    entries: T,
}

pub type MemoryCache<T> = Mutex<Option<(u64, T)>>;

pub fn cache_path(site: &str) -> PathBuf {
    Path::new(CACHE_DIR).join(format!("{}.json", site))
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// A clock that moved backwards (restored snapshot, corrected drift) reads as stale rather than as
// fresh forever, so the directory still refreshes.
pub fn is_stale(fetched_at: u64) -> bool {
    now_secs().saturating_sub(fetched_at) >= REFRESH_INTERVAL_SECS || fetched_at > now_secs()
}

// Serve `site`'s directory from memory, then disk, and only fetch inline when neither has a copy.
pub async fn cached<T, F, Fut>(
    site: &'static str,
    memory: &'static MemoryCache<T>,
    fetch: F,
) -> Result<T, String>
where
    T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
    F: Fn() -> Fut + Send + Sync + Copy + 'static,
    Fut: Future<Output = Result<T, String>> + Send,
{
    if let Some((fetched_at, entries)) = memory.lock().await.clone() {
        if is_stale(fetched_at) {
            spawn_refresh(site, memory, fetch);
        }
        return Ok(entries);
    }

    if let Some((fetched_at, entries)) = load::<T>(site).await {
        *memory.lock().await = Some((fetched_at, entries.clone()));
        if is_stale(fetched_at) {
            spawn_refresh(site, memory, fetch);
        }
        return Ok(entries);
    }

    let entries = fetch().await?;
    commit(site, memory, entries.clone()).await;
    Ok(entries)
}

// Replace a directory now and wait for it, for `/refreshfansubs`-style explicit refreshes.
pub async fn refresh_now<T, F, Fut>(
    site: &'static str,
    memory: &'static MemoryCache<T>,
    fetch: F,
) -> Result<T, String>
where
    T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let entries = fetch().await?;
    commit(site, memory, entries.clone()).await;
    Ok(entries)
}

fn spawn_refresh<T, F, Fut>(site: &'static str, memory: &'static MemoryCache<T>, fetch: F)
where
    T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, String>> + Send,
{
    if !begin_refresh(site) {
        return;
    }
    tokio::spawn(async move {
        match fetch().await {
            Ok(entries) => commit(site, memory, entries).await,
            // The previous copy stays in place; an outdated selector beats an empty one.
            Err(e) => eprintln!("[{}] directory refresh failed, keeping cached copy: {}", site, e),
        }
        end_refresh(site);
    });
}

fn begin_refresh(site: &'static str) -> bool {
    let mut guard = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(HashSet::new).insert(site)
}

fn end_refresh(site: &'static str) {
    let mut guard = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(sites) = guard.as_mut() {
        sites.remove(site);
    }
}

async fn commit<T>(site: &str, memory: &MemoryCache<T>, entries: T)
where
    T: Clone + Serialize,
{
    let fetched_at = now_secs();
    *memory.lock().await = Some((fetched_at, entries.clone()));
    if let Err(e) = store(site, fetched_at, &entries).await {
        eprintln!("[{}] directory cache write failed: {}", site, e);
    }
}

async fn load<T: DeserializeOwned>(site: &str) -> Option<(u64, T)> {
    let text = tokio::fs::read_to_string(cache_path(site)).await.ok()?;
    let stored: Stored<T> = serde_json::from_str(&text).ok()?;
    Some((stored.fetched_at, stored.entries))
}

// Written through a temp file and renamed, like the AnimeciX session cache, so a crash mid-write
// cannot leave a half-written directory that parses as an empty selector.
async fn store<T: Serialize>(site: &str, fetched_at: u64, entries: &T) -> Result<(), String> {
    let path = cache_path(site);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_vec(&Stored {
        fetched_at,
        entries,
    })
    .map_err(|e| e.to_string())?;
    let temp_path = path.with_extension("json.tmp");
    tokio::fs::remove_file(&temp_path).await.ok();
    let mut file = tokio::fs::File::create(&temp_path)
        .await
        .map_err(|e| e.to_string())?;
    file.write_all(&json).await.map_err(|e| e.to_string())?;
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    tokio::fs::rename(&temp_path, &path)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_is_measured_against_the_refresh_interval() {
        let now = now_secs();
        assert!(!is_stale(now));
        assert!(!is_stale(now - REFRESH_INTERVAL_SECS + 60));
        assert!(is_stale(now - REFRESH_INTERVAL_SECS));
        assert!(is_stale(now - REFRESH_INTERVAL_SECS * 2));
    }

    #[test]
    fn a_timestamp_from_the_future_is_stale_rather_than_permanently_fresh() {
        assert!(is_stale(now_secs() + REFRESH_INTERVAL_SECS));
    }

    #[test]
    fn only_the_first_caller_starts_a_refresh() {
        assert!(begin_refresh("directory-test-site"));
        assert!(!begin_refresh("directory-test-site"));
        end_refresh("directory-test-site");
        assert!(begin_refresh("directory-test-site"));
        end_refresh("directory-test-site");
    }

    #[tokio::test]
    async fn a_stored_directory_round_trips_with_its_timestamp() {
        let site = "directory-test-roundtrip";
        let entries = vec!["akira-subs".to_string(), "luminasubs".to_string()];
        store(site, 1_700_000_000, &entries).await.unwrap();

        let (fetched_at, loaded): (u64, Vec<String>) = load(site).await.unwrap();
        assert_eq!(fetched_at, 1_700_000_000);
        assert_eq!(loaded, entries);

        tokio::fs::remove_file(cache_path(site)).await.ok();
    }

    #[tokio::test]
    async fn a_corrupt_cache_file_reads_as_absent_instead_of_failing() {
        let site = "directory-test-corrupt";
        let path = cache_path(site);
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&path, b"{ not json").await.unwrap();

        assert!(load::<Vec<String>>(site).await.is_none());

        tokio::fs::remove_file(&path).await.ok();
    }
}
