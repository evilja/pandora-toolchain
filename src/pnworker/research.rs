// Caramel's research worker: what the machine does for the CRF model while it has no release to
// make.
//
// When the queue is empty and has been for a while, the worker takes one source nobody has
// measured yet, runs `pnmpeg --caramel-lab` on it — scenes encoded at several CRFs without
// subtitles and measured against the source — and adds the result to `DB/caramel/dataset/`. Once
// enough new scenes have come in it runs `pnmpeg --caramel-train --train-install`, which fits a
// model to the whole dataset and installs it only if it is no worse than the one in use on scenes
// that one has never seen. Then it takes the next source.
//
// A release always comes first. The lab runs at the lowest priority, and the moment the queue has
// a job in it the worker asks the lab to stop, waits a little, and kills it; a scene that was half
// measured is measured again next time and nothing else is lost. Nothing here touches a job, the
// queue, or Discord: the worker only reads whether the queue is empty.
//
// Sources come from four places, none of them used unless the operator turns the worker on:
// files dropped into `DB/caramel/sources/`, the inputs of past jobs still in the input cache,
// releases an operator queued over the API with the series they belong to, and — only when a feed
// is named — releases found on an RSS feed. Anything downloaded is deleted once measured. A series is measured once or twice and then left alone: a model learns
// from the fortieth show, not from the fortieth episode of one.

use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{CARAMEL_RESEARCH, CARAMEL_RESEARCH_CRFS, CARAMEL_RESEARCH_FEED, CARAMEL_RESEARCH_PRESET, CARAMEL_RESEARCH_RETRAIN, CARAMEL_RESEARCH_SCENES, PNMPEG, PNP2P};
use crate::lib::mpeg::caramel_lab::{read_dataset, series_key};
use crate::lib::mpeg::caramel_train::{DATASET_DIRECTORY, installed_version};
use crate::lib::p2p::nyaaise::{TorrentType, nyaaise};
use crate::lib::watch::FeedItem;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SOURCES_DIRECTORY: &str = "DB/caramel/sources";
pub const STATE_FILE: &str = "DB/caramel/research.json";
const FETCH_DIRECTORY: &str = "DB/caramel/sources/fetched";
// Releases queued over the API, one directory per request, and the queue itself.
const REQUEST_DIRECTORY: &str = "DB/caramel/sources/requested";
pub const REQUESTS_FILE: &str = "DB/caramel/requests.json";
const LOG_DIRECTORY: &str = "DB/caramel/log";
const FOREGROUND_MARKER: &str = "DB/work/.foreground-encode";

// How often the worker looks at the queue, and how long the queue must have been empty before it
// starts: a queue that empties between two episodes of a batch is not an idle machine.
const POLL: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(300);
// How long a lab asked to stop is given before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(30);
// Sources of one series the dataset takes.
const SERIES_SOURCES: usize = 2;
// What a feed item must be to be worth downloading: one episode, not a batch and not a clip.
const FETCH_BYTES: (u64, u64) = (150 << 20, 4 << 30);
const FETCH_SEEDERS: u32 = 5;
const FETCH_TIMEOUT: Duration = Duration::from_secs(45 * 60);
const TORRENT_MAX_BYTES: usize = 4 << 20;
// A feed that was just read is not read again for this long, however often the worker runs dry.
const FEED_INTERVAL_SECS: u64 = 1800;
const FEED_SEEN: usize = 2000;
const VIDEO_EXTENSIONS: [&str; 4] = ["mkv", "mp4", "m2ts", "webm"];
// A queued release is a file the operator chose, often a film or a BD remux: it gets longer than a
// feed item, and a download that fails is tried again before it is given up on. One stopped for
// the queue is resumed, and does not count.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3 * 60 * 60);
const REQUEST_ATTEMPTS: u32 = 3;
const REQUESTS_KEPT: usize = 500;

#[derive(Clone, Debug, PartialEq)]
pub struct ResearchConfig {
    pub enabled: bool,
    pub feed: Option<String>,
    pub scenes: usize,
    // New scenes between two trainings.
    pub retrain: usize,
    pub preset: String,
    // The CRFs every scene is encoded at, or empty for the lab to search each scene's curve.
    pub crfs: Vec<f64>,
}

// CRFs as `env.pandora` and the API spell them: numbers between 0 and 51, comma- or
// space-separated, in order and once each.
pub fn parse_crfs(text: &str) -> Vec<f64> {
    let mut crfs: Vec<f64> = text.split(|c: char| c == ',' || c.is_whitespace()).filter_map(|value| value.trim().parse::<f64>().ok()).filter(|crf| crf.is_finite() && (0.0..=51.0).contains(crf)).collect();
    crfs.sort_by(f64::total_cmp);
    crfs.dedup();
    crfs
}

// The worker's settings out of `env.pandora`. Off unless `caramel_research` says on.
pub fn research_config(env: &HashMap<String, String>) -> ResearchConfig {
    let number = |key: &str, default: usize, low: usize, high: usize| env.get(key).and_then(|value| value.trim().parse::<usize>().ok()).unwrap_or(default).clamp(low, high);
    ResearchConfig {
        enabled: env.get(CARAMEL_RESEARCH).is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "on" | "true" | "1" | "yes")),
        feed: env.get(CARAMEL_RESEARCH_FEED).map(|value| value.trim().to_string()).filter(|value| !value.is_empty()),
        scenes: number(CARAMEL_RESEARCH_SCENES, 40, 4, 400),
        retrain: number(CARAMEL_RESEARCH_RETRAIN, 200, 20, 100_000),
        preset: env.get(CARAMEL_RESEARCH_PRESET).map(|value| value.trim().to_ascii_lowercase()).filter(|value| !value.is_empty()).unwrap_or_else(|| "standard".to_string()),
        crfs: env.get(CARAMEL_RESEARCH_CRFS).map(|value| parse_crfs(value)).unwrap_or_default(),
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SourceRecord {
    pub series: String,
    pub finished_unix: u64,
    // `measured`, or why the source gave nothing: `failed`, `unreadable`.
    pub outcome: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ResearchState {
    // Every source the worker is done with, by its id in the dataset.
    #[serde(default)]
    pub sources: BTreeMap<String, SourceRecord>,
    // Feed items already downloaded or given up on, oldest first.
    #[serde(default)]
    pub feed_seen: Vec<String>,
    #[serde(default)]
    pub feed_read_unix: u64,
    // Scenes in the dataset when a training last ran, and what it said.
    #[serde(default)]
    pub trained_scenes: usize,
    #[serde(default)]
    pub last_training: Option<Value>,
    // What the worker is doing, for whoever asks.
    #[serde(default)]
    pub activity: String,
    #[serde(default)]
    pub updated_unix: u64,
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_secs()).unwrap_or(0)
}

pub fn load_state() -> ResearchState {
    std::fs::read_to_string(STATE_FILE).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn save_state(state: &mut ResearchState, activity: &str) {
    state.activity = activity.to_string();
    state.updated_unix = unix_now();
    let scratch = format!("{STATE_FILE}.tmp");
    if std::fs::create_dir_all(Path::new(STATE_FILE).parent().unwrap()).is_ok() && std::fs::write(&scratch, serde_json::to_string_pretty(state).unwrap()).is_ok() {
        let _ = std::fs::rename(&scratch, STATE_FILE);
    }
}

// What the API reports about the worker: whether it is on, what it is doing, and what it has.
pub fn status() -> Value {
    let config = research_config(&get_pandora_env());
    let state = load_state();
    let samples = read_dataset(Path::new(DATASET_DIRECTORY));
    let mut series: Vec<&str> = samples.iter().map(|sample| sample.series.as_str()).collect();
    series.sort_unstable();
    series.dedup();
    json!({
        "enabled": config.enabled,
        "feed": config.feed.is_some(),
        "activity": state.activity,
        "updated_at": state.updated_unix,
        "model": installed_version(),
        "scenes": samples.len(),
        "series": series.len(),
        "sources_done": state.sources.len(),
        "scenes_at_last_training": state.trained_scenes,
        "retrain_after": config.retrain,
        "last_training": state.last_training,
        // How many queued releases are in each state; the releases themselves are the operator's.
        "requests": request_counts(&load_requests()),
    })
}

// A release an operator queued for the worker: a torrent, the file in it, and the series it is.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SourceRequest {
    pub id: String,
    // A `.torrent` link over https or a magnet, as `nyaaise` makes it.
    pub link: String,
    #[serde(default)]
    pub file_index: Option<u64>,
    pub series: String,
    #[serde(default)]
    pub added_unix: u64,
    // `queued`, `downloading`, `downloaded`, `measuring`, then `measured` or `failed`.
    pub status: String,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub error: Option<String>,
    // The source's id in the dataset, once it is known.
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub finished_unix: u64,
}

impl SourceRequest {
    fn finished(&self) -> bool {
        matches!(self.status.as_str(), "measured" | "failed")
    }
}

// The API adds to the queue while the worker moves its entries along, both in this process: every
// change is a read, an edit and a rename under this lock, so neither loses the other's.
static REQUESTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn load_requests() -> Vec<SourceRequest> {
    std::fs::read_to_string(REQUESTS_FILE).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

// Changes the queue and writes it back. A queue that cannot be written is an error the caller
// reports; what `change` returned is kept either way.
pub fn with_requests<T>(change: impl FnOnce(&mut Vec<SourceRequest>) -> T) -> Result<T, String> {
    let _guard = REQUESTS_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut requests = load_requests();
    let out = change(&mut requests);
    // The oldest finished entries go first once the list is long; a queued one is never dropped.
    while requests.len() > REQUESTS_KEPT {
        let Some(oldest) = requests.iter().position(SourceRequest::finished) else { break };
        requests.remove(oldest);
    }
    let scratch = format!("{REQUESTS_FILE}.tmp");
    std::fs::create_dir_all(Path::new(REQUESTS_FILE).parent().unwrap()).map_err(|error| error.to_string())?;
    std::fs::write(&scratch, serde_json::to_string_pretty(&requests).unwrap()).map_err(|error| error.to_string())?;
    std::fs::rename(&scratch, REQUESTS_FILE).map_err(|error| error.to_string())?;
    Ok(out)
}

fn request_counts(requests: &[SourceRequest]) -> Value {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for request in requests {
        *counts.entry(request.status.as_str()).or_insert(0) += 1;
    }
    json!(counts)
}

// A series name as the dataset keeps it: lowercase, one space between words, and short.
pub fn clean_series(series: &str) -> Option<String> {
    let cleaned = series.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    (!cleaned.is_empty() && cleaned.chars().count() <= 80 && !cleaned.chars().any(char::is_control)).then_some(cleaned)
}

// Queues a release. The link must be a torrent the worker can fetch: a `.torrent` over https (a
// nyaa view page is turned into its download link) or a magnet. The same file of the same torrent
// is not queued twice while the first is still to be done or was measured.
pub fn add_request(link: &str, file_index: Option<u64>, series: &str) -> Result<SourceRequest, String> {
    let series = clean_series(series).ok_or("series must be a name of 1 to 80 characters")?;
    let link = match nyaaise(link) {
        TorrentType::Link(url) if url.starts_with("https://") => url,
        TorrentType::Magnet(magnet) => magnet,
        _ => return Err("torrent must be a .torrent link over https, a nyaa page, or a magnet".to_string()),
    };
    let added_unix = unix_now();
    with_requests(|requests| {
        if let Some(existing) = requests.iter().find(|request| request.link == link && request.file_index == file_index && request.status != "failed") {
            return Err(format!("already queued as {} ({})", existing.id, existing.status));
        }
        let digest = format!("{:x}", md5::compute(format!("{link}|{file_index:?}|{added_unix}|{}", requests.len())));
        let request = SourceRequest { id: digest[..12].to_string(), link: link.clone(), file_index, series: series.clone(), added_unix, status: "queued".to_string(), ..Default::default() };
        requests.push(request.clone());
        Ok(request)
    })?
}

// Takes a release off the queue and deletes whatever was downloaded for it. One the worker is
// downloading or measuring right now is left alone: it has to be stopped by a job first.
pub fn remove_request(id: &str) -> Result<bool, String> {
    let removed = with_requests(|requests| {
        let Some(position) = requests.iter().position(|request| request.id == id) else {
            return Ok(false);
        };
        if matches!(requests[position].status.as_str(), "downloading" | "measuring") {
            return Err(format!("{id} is {} right now", requests[position].status));
        }
        requests.remove(position);
        Ok(true)
    })??;
    if removed && is_request_id(id) {
        let _ = std::fs::remove_dir_all(Path::new(REQUEST_DIRECTORY).join(id));
    }
    Ok(removed)
}

// What `add_request` makes: twelve lowercase hex digits, safe to join onto a path.
pub fn is_request_id(id: &str) -> bool {
    id.len() == 12 && id.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn update_request(id: &str, change: impl FnOnce(&mut SourceRequest)) {
    let result = with_requests(|requests| {
        if let Some(request) = requests.iter_mut().find(|request| request.id == id) {
            change(request);
        }
    });
    if let Err(error) = result {
        println!("[Caramel research] the request queue could not be written: {error}");
    }
}

// A source the worker could measure.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub path: PathBuf,
    pub source_id: String,
    pub series: String,
    // Downloaded for this and nothing else: removed once measured.
    pub fetched: bool,
}

fn is_video(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| VIDEO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

fn videos_under(directory: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
    paths.sort();
    for path in paths {
        let hidden = path.file_name().and_then(|name| name.to_str()).is_none_or(|name| name.starts_with('.'));
        if hidden {
            continue;
        }
        if path.is_dir() {
            if depth > 0 {
                videos_under(&path, depth - 1, out);
            }
        } else if is_video(&path) && path.metadata().is_ok_and(|meta| meta.len() >= FETCH_BYTES.0 / 4) {
            out.push(path);
        }
    }
}

// Every source on this machine, in the order they would be taken: the operator's own files first,
// then what was downloaded for this, then the inputs of past jobs.
pub fn local_candidates(sources: &Path, cache: &Path) -> Vec<Candidate> {
    let mut files = Vec::new();
    videos_under(sources, 4, &mut files);
    let fetched_root = sources.join("fetched");
    // A queued release is measured as its request says, with the series it was queued as.
    let requested_root = sources.join("requested");
    let mut out: Vec<Candidate> = files
        .into_iter()
        .filter(|path| !path.starts_with(&requested_root))
        .map(|path| {
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_string();
            Candidate { fetched: path.starts_with(&fetched_root), series: series_key(&name), source_id: name, path }
        })
        .collect();
    out.sort_by_key(|candidate| candidate.fetched);
    // A cached input has no name of its own — every one is `input.mkv` under a hash — and the same
    // input sits there once per key it was asked for by. Its length tells two of them apart well
    // enough, and it is its own series: nothing says which show it was.
    let mut cached = Vec::new();
    videos_under(cache, 1, &mut cached);
    for path in cached {
        let Ok(meta) = path.metadata() else { continue };
        let source_id = format!("cached-input-{}", meta.len());
        if out.iter().all(|other| other.source_id != source_id) {
            out.push(Candidate { path, series: source_id.clone(), source_id, fetched: false });
        }
    }
    out
}

// How many sources of each series the worker is done with or the dataset already holds.
fn series_counts(state: &ResearchState, dataset_series: &[(String, String)]) -> HashMap<String, usize> {
    let mut sources: BTreeMap<&str, &str> = dataset_series.iter().map(|(source, series)| (source.as_str(), series.as_str())).collect();
    for (source, record) in &state.sources {
        sources.entry(source.as_str()).or_insert(record.series.as_str());
    }
    let mut counts = HashMap::new();
    for series in sources.values() {
        *counts.entry(series.to_string()).or_insert(0) += 1;
    }
    counts
}

// The next source to measure: the first that is not done and whose series has room.
pub fn pick(candidates: &[Candidate], state: &ResearchState, dataset_series: &[(String, String)]) -> Option<Candidate> {
    let counts = series_counts(state, dataset_series);
    candidates
        .iter()
        .find(|candidate| {
            // A source the dataset holds scenes of but the worker never finished is carried on with.
            let started = dataset_series.iter().any(|(source, _)| *source == candidate.source_id);
            !state.sources.contains_key(&candidate.source_id) && (started || counts.get(&candidate.series).copied().unwrap_or(0) < SERIES_SOURCES)
        })
        .cloned()
}

// The feed item to download next: a single 1080p episode with seeders, of a series the dataset has
// no room left for skipped, and nothing that was tried before.
pub fn pick_feed_item(items: &[FeedItem], state: &ResearchState, dataset_series: &[(String, String)]) -> Option<FeedItem> {
    let counts = series_counts(state, dataset_series);
    items
        .iter()
        .find(|item| {
            let sized = item.size_bytes.is_some_and(|bytes| (FETCH_BYTES.0..=FETCH_BYTES.1).contains(&bytes));
            let seeded = item.seeders.is_some_and(|seeders| seeders >= FETCH_SEEDERS);
            let full_hd = item.title.contains("1080");
            let torrent = item.link.starts_with("https://") && item.link.ends_with(".torrent");
            sized && seeded && full_hd && torrent && !state.feed_seen.contains(&item.key) && counts.get(&series_key(&format!("{}.mkv", item.title))).copied().unwrap_or(0) < SERIES_SOURCES
        })
        .cloned()
}

// Whether the machine has nothing to do for a release: the worker loop has said so and no encode
// holds the foreground marker. Before the loop has published anything, it is not idle.
fn queue_idle() -> bool {
    let empty = crate::pnworker::snapshot::current().and_then(|snapshot| snapshot.get("queue_len").and_then(Value::as_u64)) == Some(0);
    empty && !Path::new(FOREGROUND_MARKER).exists()
}

fn slug(text: &str) -> String {
    let mut out: String = text.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-');
    let digest = format!("{:x}", md5::compute(text));
    format!("{}-{}", &out[..out.len().min(60)], &digest[..8])
}

// pnp2p as a child, routed the way a job's is: through Pandora's torrent proxy (`PNP2P_PROXY`)
// while the worker's health check passes, directly while it fails.
fn torrent_command(pnp2p: &str) -> tokio::process::Command {
    let mut command = background_command(pnp2p);
    crate::pnworker::proxy_health::apply_to_command(&mut command);
    command
}

// A tool as a child at the lowest priority, with nothing inherited to write to.
fn background_command(tool: &str) -> tokio::process::Command {
    #[cfg(unix)]
    let mut command = {
        let mut command = tokio::process::Command::new("nice");
        command.args(["-n", "19", tool]);
        command
    };
    #[cfg(not(unix))]
    let mut command = tokio::process::Command::new(tool);
    command.stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    command
}

enum Run {
    // The tool ended by itself, with this exit code and this on its standard output.
    Ended(i32, String),
    // The queue filled, or the time ran out, and the tool was stopped.
    Stopped,
}

// Runs a tool until it ends, the queue has a job, or `limit` passes. The tool is asked to stop
// through its cancel file first.
async fn run_while_idle(mut command: tokio::process::Command, cancel_file: &Path, limit: Option<Duration>) -> Run {
    let _ = std::fs::remove_file(cancel_file);
    command.stdout(Stdio::piped());
    let Ok(mut child) = command.spawn() else {
        return Run::Ended(-1, String::new());
    };
    let mut stdout = child.stdout.take();
    let reader = tokio::spawn(async move {
        let mut text = String::new();
        if let Some(stdout) = stdout.as_mut() {
            let _ = tokio::io::AsyncReadExt::read_to_string(stdout, &mut text).await;
        }
        text
    });
    let started = std::time::Instant::now();
    loop {
        tokio::select! {
            status = child.wait() => {
                let code = status.ok().and_then(|status| status.code()).unwrap_or(-1);
                return Run::Ended(code, reader.await.unwrap_or_default());
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                if queue_idle() && limit.is_none_or(|limit| started.elapsed() < limit) {
                    continue;
                }
                let _ = std::fs::write(cancel_file, b"research\n");
                if tokio::time::timeout(STOP_GRACE, child.wait()).await.is_err() {
                    let _ = child.kill().await;
                }
                let _ = std::fs::remove_file(cancel_file);
                return Run::Stopped;
            }
        }
    }
}

fn tool(env: &HashMap<String, String>, key: &str) -> Option<String> {
    env.get(key).map(|path| path.trim().to_string()).filter(|path| !path.is_empty())
}

// The last line of a tool's output that is a JSON object: its summary.
fn summary_json(output: &str) -> Option<Value> {
    output.lines().rev().find_map(|line| serde_json::from_str::<Value>(line.trim()).ok().filter(Value::is_object))
}

// Measures one source. Returns how it went — `measured` or `failed`, with pnmpeg's reason when it
// gave one — or None when it was stopped for the queue and is to be carried on with.
async fn measure(candidate: &Candidate, config: &ResearchConfig, pnmpeg: &str, state: &mut ResearchState) -> Option<(&'static str, Option<String>)> {
    save_state(state, &format!("measuring {}", candidate.source_id));
    let _ = std::fs::create_dir_all(LOG_DIRECTORY);
    let name = slug(&candidate.source_id);
    let cancel_file = Path::new(LOG_DIRECTORY).join(format!("{name}.cancel"));
    let mut command = background_command(pnmpeg);
    command.args(["--caramel-lab", "--preset", &config.preset, "--lab-scenes", &config.scenes.to_string(), "--lab-source", &candidate.source_id, "--lab-series", &candidate.series]);
    if !config.crfs.is_empty() {
        command.arg("--lab-crfs").arg(config.crfs.iter().map(|crf| crf.to_string()).collect::<Vec<_>>().join(","));
    }
    command.arg("-i").arg(&candidate.path);
    command.arg("-o").arg(Path::new(DATASET_DIRECTORY).join(format!("{name}.jsonl")));
    command.arg("--cancelfile").arg(&cancel_file);
    command.arg("--logfile").arg(Path::new(LOG_DIRECTORY).join(format!("{name}.log")));
    let mut reason = None;
    let outcome = match run_while_idle(command, &cancel_file, None).await {
        Run::Stopped => {
            println!("[Caramel research] stopped measuring {} for the queue", candidate.source_id);
            // A lab that had to be killed leaves the clips it was working on: gigabytes a release
            // may need. The lab names its scratch after the dataset file.
            let _ = std::fs::remove_dir_all(Path::new(DATASET_DIRECTORY).join(format!(".caramel-lab-{name}")));
            return None;
        }
        // Stopped by its own cancel file before the kill.
        Run::Ended(3, _) => return None,
        Run::Ended(0, output) => {
            let measured = summary_json(&output).and_then(|summary| summary.get("measured").and_then(Value::as_u64)).unwrap_or(0);
            println!("[Caramel research] measured {measured} scenes of {}", candidate.source_id);
            "measured"
        }
        Run::Ended(code, output) => {
            println!("[Caramel research] {} could not be measured (pnmpeg exit {code})", candidate.source_id);
            reason = summary_json(&output).and_then(|summary| summary.get("error").and_then(Value::as_str).map(str::to_string));
            "failed"
        }
    };
    state.sources.insert(candidate.source_id.clone(), SourceRecord { series: candidate.series.clone(), finished_unix: unix_now(), outcome: outcome.to_string() });
    if candidate.fetched {
        // The directory the download was saved into, directly under `fetched/` or `requested/`.
        let download_root = |parent: &Path| parent == Path::new(FETCH_DIRECTORY) || parent == Path::new(REQUEST_DIRECTORY);
        if let Some(directory) = candidate.path.ancestors().find(|ancestor| ancestor.parent().is_some_and(download_root)) {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
    Some((outcome, reason))
}

// The video a finished download is: the largest one under its directory.
fn downloaded_video(directory: &Path) -> Option<PathBuf> {
    let mut files = Vec::new();
    videos_under(directory, 4, &mut files);
    files.into_iter().max_by_key(|path| path.metadata().map(|meta| meta.len()).unwrap_or(0))
}

// Moves the queue along by one step: measures a release that has been downloaded, else downloads
// the next one. None when the queue has nothing to do; otherwise whether the step finished.
async fn work_on_requests(config: &ResearchConfig, env: &HashMap<String, String>, pnmpeg: &str, state: &mut ResearchState) -> Option<bool> {
    let requests = load_requests();
    // `measuring` and `downloading` are what a restart interrupted.
    if let Some(request) = requests.iter().find(|request| matches!(request.status.as_str(), "downloaded" | "measuring")) {
        return Some(measure_request(request, config, pnmpeg, state).await);
    }
    let request = requests.iter().find(|request| matches!(request.status.as_str(), "queued" | "downloading"))?;
    Some(download_request(request, env, state).await)
}

async fn measure_request(request: &SourceRequest, config: &ResearchConfig, pnmpeg: &str, state: &mut ResearchState) -> bool {
    let directory = Path::new(REQUEST_DIRECTORY).join(&request.id);
    let Some(path) = downloaded_video(&directory) else {
        update_request(&request.id, |request| {
            request.status = "failed".to_string();
            request.error = Some("the download holds no video".to_string());
            request.finished_unix = unix_now();
        });
        let _ = std::fs::remove_dir_all(&directory);
        return true;
    };
    let source_id = path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_string();
    update_request(&request.id, |request| {
        request.status = "measuring".to_string();
        request.source_id = Some(source_id.clone());
    });
    // Asked for by name, so the series' two-source limit does not apply. A source the worker has
    // already measured is not measured again; one that failed is tried again, since asking for it
    // again is how a failure is retried.
    let outcome = if state.sources.get(&source_id).is_some_and(|record| record.outcome == "measured") {
        let _ = std::fs::remove_dir_all(&directory);
        Some(("measured", None))
    } else {
        let candidate = Candidate { path, source_id: source_id.clone(), series: request.series.clone(), fetched: true };
        measure(&candidate, config, pnmpeg, state).await
    };
    update_request(&request.id, |request| match &outcome {
        Some((outcome, reason)) => {
            request.status = outcome.to_string();
            request.error = (*outcome == "failed").then(|| match reason {
                Some(reason) => format!("pnmpeg could not measure it: {reason}"),
                None => "pnmpeg could not measure it; see DB/caramel/log/".to_string(),
            });
            request.finished_unix = unix_now();
        }
        None => request.status = "downloaded".to_string(),
    });
    outcome.is_some()
}

async fn download_request(request: &SourceRequest, env: &HashMap<String, String>, state: &mut ResearchState) -> bool {
    let Some(pnp2p) = tool(env, PNP2P) else {
        return false;
    };
    let directory = Path::new(REQUEST_DIRECTORY).join(&request.id);
    if !is_request_id(&request.id) || std::fs::create_dir_all(&directory).is_err() {
        return false;
    }
    update_request(&request.id, |request| request.status = "downloading".to_string());
    save_state(state, &format!("downloading request {} ({})", request.id, request.series));
    let _ = std::fs::create_dir_all(LOG_DIRECTORY);
    let cancel_file = Path::new(LOG_DIRECTORY).join(format!("request-{}.cancel", request.id));
    let mut command = torrent_command(&pnp2p);
    let failure = match nyaaise(&request.link) {
        TorrentType::Link(url) => {
            let torrent = directory.join("request.torrent");
            let fetched = if torrent.exists() { Ok(()) } else { fetch_torrent(&url, &torrent).await };
            command.arg("--opcode").arg(&torrent).arg("--nomagnet");
            fetched.err()
        }
        TorrentType::Magnet(magnet) => {
            command.arg("--opcode").arg(magnet).arg("--magnet");
            None
        }
        _ => Some("not a torrent".to_string()),
    };
    let failure = match failure {
        Some(error) => Some(error),
        None => {
            if let Some(index) = request.file_index {
                command.arg("--select").arg(index.to_string());
            }
            command.arg("--save").arg(&directory);
            command.args(["--tag", "caramel-research"]);
            command.arg("--cancelfile").arg(&cancel_file);
            command.arg("--logfile").arg(Path::new(LOG_DIRECTORY).join(format!("request-{}.log", request.id)));
            match run_while_idle(command, &cancel_file, Some(REQUEST_TIMEOUT)).await {
                Run::Ended(0, _) => {
                    println!("[Caramel research] downloaded request {} ({})", request.id, request.series);
                    update_request(&request.id, |request| request.status = "downloaded".to_string());
                    return true;
                }
                // A job came in: what was downloaded stays, and the download resumes next time.
                Run::Stopped if !queue_idle() => {
                    update_request(&request.id, |request| request.status = "queued".to_string());
                    return false;
                }
                Run::Stopped => Some(format!("not finished in {} hours", REQUEST_TIMEOUT.as_secs() / 3600)),
                Run::Ended(code, _) => Some(format!("pnp2p exit {code}")),
            }
        }
    };
    let error = failure.unwrap_or_default();
    println!("[Caramel research] request {} did not download: {error}", request.id);
    let mut gave_up = false;
    update_request(&request.id, |request| {
        request.attempts += 1;
        request.error = Some(error.clone());
        gave_up = request.attempts >= REQUEST_ATTEMPTS;
        request.status = if gave_up { "failed" } else { "queued" }.to_string();
        if gave_up {
            request.finished_unix = unix_now();
        }
    });
    if gave_up {
        let _ = std::fs::remove_dir_all(&directory);
    }
    true
}

// Fits a model to the dataset and installs it if it may replace the one in use. It is trained for
// the preset the worker measures with: scenes that were encoded with other settings — a dataset
// copied from another machine, or one from before the preset was edited — are left out, however
// many of them there are.
async fn retrain(pnmpeg: &str, preset: &str, scenes: usize, state: &mut ResearchState) {
    save_state(state, "training");
    let _ = std::fs::create_dir_all(LOG_DIRECTORY);
    let cancel_file = Path::new(LOG_DIRECTORY).join("train.cancel");
    let mut command = background_command(pnmpeg);
    command.args(["--caramel-train", "--train-install", "--preset", preset, "-i", DATASET_DIRECTORY, "-o", "-"]);
    command.arg("--logfile").arg(Path::new(LOG_DIRECTORY).join("train.log"));
    match run_while_idle(command, &cancel_file, Some(Duration::from_secs(1800))).await {
        // Training is seconds to minutes and has no cancel file to read: a stop is a kill, and it
        // is run again when the machine is next idle.
        Run::Stopped => {}
        Run::Ended(code, output) => {
            state.trained_scenes = scenes;
            let report = summary_json(&output);
            let line = report.as_ref().map(|report| {
                format!(
                    "{} from {} scenes: {}{}",
                    report.get("version").and_then(Value::as_str).unwrap_or("?"),
                    report.get("scenes").and_then(Value::as_u64).unwrap_or(0),
                    report.get("reason").and_then(Value::as_str).unwrap_or(""),
                    if report.get("installed").and_then(Value::as_bool) == Some(true) { " — installed" } else { "" },
                )
            });
            println!("[Caramel research] training ended ({code}): {}", line.unwrap_or_else(|| "no report".to_string()));
            state.last_training = report.map(|mut report| {
                // The per-metric tables are in the log; the state keeps what a status needs.
                if let Some(object) = report.as_object_mut() {
                    object.remove("metrics");
                    object.insert("at".to_string(), json!(unix_now()));
                }
                report
            });
        }
    }
}

async fn fetch_torrent(link: &str, target: &Path) -> Result<(), String> {
    let url = crate::lib::http::net::sanitize_fetch_url(link).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(40))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Pandora/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())?;
    let mut last = String::new();
    for _ in 0..4 {
        match client.get(&url).send().await {
            Ok(mut response) if response.status().is_success() => {
                let mut body = Vec::new();
                while let Ok(Some(chunk)) = response.chunk().await {
                    body.extend_from_slice(&chunk);
                    if body.len() > TORRENT_MAX_BYTES {
                        return Err("that is too large to be a .torrent".to_string());
                    }
                }
                // A bencoded dictionary, or it is an error page.
                if body.first() == Some(&b'd') {
                    return tokio::fs::write(target, &body).await.map_err(|error| error.to_string());
                }
                last = "the answer was not a .torrent".to_string();
            }
            Ok(response) => last = format!("the tracker answered {}", response.status()),
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    Err(last)
}

// Finds a release on the feed and downloads it. None when the feed has nothing new, the download
// did not finish, or the queue filled meanwhile.
async fn discover(feed: &str, env: &HashMap<String, String>, state: &mut ResearchState, dataset_series: &[(String, String)]) -> Option<()> {
    if unix_now().saturating_sub(state.feed_read_unix) < FEED_INTERVAL_SECS {
        return None;
    }
    let pnp2p = tool(env, PNP2P)?;
    state.feed_read_unix = unix_now();
    save_state(state, "reading the feed");
    let items = match crate::lib::watch::fetch_feed(feed).await {
        Ok(items) => items,
        Err(error) => {
            println!("[Caramel research] the feed could not be read: {error}");
            return None;
        }
    };
    let item = pick_feed_item(&items, state, dataset_series)?;
    // Tried once, whatever comes of it: a release that will not download is not asked for again.
    state.feed_seen.push(item.key.clone());
    if state.feed_seen.len() > FEED_SEEN {
        let extra = state.feed_seen.len() - FEED_SEEN;
        state.feed_seen.drain(..extra);
    }
    save_state(state, &format!("downloading {}", item.title));
    let directory = Path::new(FETCH_DIRECTORY).join(&item.key);
    tokio::fs::create_dir_all(&directory).await.ok()?;
    let _ = std::fs::create_dir_all(LOG_DIRECTORY);
    let torrent = directory.join("fetch.torrent");
    if let Err(error) = fetch_torrent(&item.link, &torrent).await {
        println!("[Caramel research] {}: {error}", item.title);
        let _ = std::fs::remove_dir_all(&directory);
        return None;
    }
    let cancel_file = Path::new(LOG_DIRECTORY).join(format!("fetch-{}.cancel", item.key));
    let mut command = torrent_command(&pnp2p);
    command.arg("--opcode").arg(&torrent).arg("--nomagnet").arg("--save").arg(&directory);
    command.args(["--tag", "caramel-research"]);
    command.arg("--cancelfile").arg(&cancel_file);
    command.arg("--logfile").arg(Path::new(LOG_DIRECTORY).join(format!("fetch-{}.log", item.key)));
    match run_while_idle(command, &cancel_file, Some(FETCH_TIMEOUT)).await {
        Run::Ended(0, _) => {
            println!("[Caramel research] downloaded {}", item.title);
            Some(())
        }
        _ => {
            println!("[Caramel research] gave up on {}", item.title);
            let _ = std::fs::remove_dir_all(&directory);
            None
        }
    }
}

// One round of work on an idle machine. Returns whether anything was done, so the loop knows
// whether to come straight back.
async fn work(config: &ResearchConfig, env: &HashMap<String, String>) -> bool {
    let Some(pnmpeg) = tool(env, PNMPEG) else {
        return false;
    };
    let mut state = load_state();
    let dataset = tokio::task::spawn_blocking(|| read_dataset(Path::new(DATASET_DIRECTORY))).await.unwrap_or_default();
    let mut dataset_series: Vec<(String, String)> = dataset.iter().map(|sample| (sample.source_id.clone(), sample.series.clone())).collect();
    dataset_series.sort();
    dataset_series.dedup();
    if dataset.len() >= state.trained_scenes + config.retrain {
        retrain(&pnmpeg, &config.preset, dataset.len(), &mut state).await;
        save_state(&mut state, "idle");
        return true;
    }
    let candidates = local_candidates(Path::new(SOURCES_DIRECTORY), &PathBuf::from("DB").join("cache").join("inputs"));
    if let Some(candidate) = pick(&candidates, &state, &dataset_series) {
        let done = measure(&candidate, config, &pnmpeg, &mut state).await.is_some();
        save_state(&mut state, "idle");
        return done;
    }
    if let Some(done) = work_on_requests(config, env, &pnmpeg, &mut state).await {
        save_state(&mut state, "idle");
        return done;
    }
    let found = match config.feed.as_deref() {
        Some(feed) => discover(feed, env, &mut state, &dataset_series).await.is_some(),
        None => false,
    };
    save_state(&mut state, if found { "idle" } else { "nothing left to measure" });
    found
}

async fn run() {
    let mut idle_since: Option<std::time::Instant> = None;
    let mut announced = false;
    loop {
        tokio::time::sleep(POLL).await;
        // Read every round, so the worker is turned on and off without a restart.
        let env = get_pandora_env();
        let config = research_config(&env);
        if !config.enabled || !queue_idle() {
            idle_since = None;
            continue;
        }
        let since = *idle_since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed() < SETTLE {
            continue;
        }
        if !announced {
            println!("[Caramel research] the queue is idle; measuring for the CRF model (model {})", installed_version());
            announced = true;
        }
        if !work(&config, &env).await {
            // Nothing to do, or a job arrived: either way there is no hurry to look again.
            tokio::time::sleep(Duration::from_secs(120)).await;
        }
    }
}

// Encodes kept for verification: `<job id>/` with the encode's `decisions`
// (`pnx264::scenes::EncodeDecisions`), a hard link to its `input.mkv`, and `entry.json`. The
// research worker re-measures their raised and doubtful scenes when idle (`verify_next`), then
// removes the entry. The inputs are capped together; the oldest go first.
pub const VERIFY_DIRECTORY: &str = "DB/caramel/verify";
const VERIFY_BYTES: u64 = 40 << 30;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct VerifyEntry {
    pub job_id: u64,
    // One series per release: every episode of a pack shares it, so training holds them out
    // together like the episodes of any other series.
    pub series: String,
    pub kept_unix: u64,
    pub model: String,
    pub scenes: usize,
}

// A Caramel encode's decisions and its input, kept for verification when the research worker is
// on. A job whose encode wrote no decisions — not under Caramel, or encoded on a linked node — is
// left alone. Nothing here can fail the job: a step that does not work leaves no entry.
pub async fn keep_for_verification(job_id: u64, directory: PathBuf, source_key: String) {
    if !research_config(&get_pandora_env()).enabled {
        return;
    }
    let decisions = pnx264::scenes::encode_decisions_path(&directory.join("work").join("output.mp4"));
    let input = directory.join("contents").join("torrent").join("input.mkv");
    let _ = tokio::task::spawn_blocking(move || {
        let kept = keep_entry(Path::new(VERIFY_DIRECTORY), job_id, &decisions, &input, &source_key, unix_now());
        if let Some(entry) = kept {
            println!("[Caramel research] kept job {job_id}'s {} scenes for verification", entry.scenes);
            trim_verify_entries(Path::new(VERIFY_DIRECTORY), VERIFY_BYTES, job_id);
        }
    })
    .await;
}

fn keep_entry(root: &Path, job_id: u64, decisions: &Path, input: &Path, source_key: &str, now: u64) -> Option<VerifyEntry> {
    let parsed = pnx264::scenes::EncodeDecisions::read(decisions)?;
    if !input.exists() {
        return None;
    }
    let directory = root.join(job_id.to_string());
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).ok()?;
    let kept = (|| {
        std::fs::copy(decisions, directory.join("decisions")).ok()?;
        let target = directory.join("input.mkv");
        if std::fs::hard_link(input, &target).is_err() {
            std::fs::copy(input, &target).ok()?;
        }
        let entry = VerifyEntry {
            job_id,
            series: format!("verify-{}", &format!("{:x}", md5::compute(source_key))[..10]),
            kept_unix: now,
            model: parsed.model.clone(),
            scenes: parsed.records.len(),
        };
        std::fs::write(directory.join("entry.json"), serde_json::to_string_pretty(&entry).ok()?).ok()?;
        Some(entry)
    })();
    if kept.is_none() {
        let _ = std::fs::remove_dir_all(&directory);
    }
    kept
}

pub fn verify_entries(root: &Path) -> Vec<(PathBuf, VerifyEntry)> {
    let Ok(read) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut entries: Vec<(PathBuf, VerifyEntry)> = read
        .flatten()
        .map(|entry| entry.path())
        .filter_map(|path| {
            let entry: VerifyEntry = serde_json::from_str(&std::fs::read_to_string(path.join("entry.json")).ok()?).ok()?;
            Some((path, entry))
        })
        .collect();
    entries.sort_by_key(|(_, entry)| (entry.kept_unix, entry.job_id));
    entries
}

// Drops the oldest entries, never `keep`, until their inputs fit in `limit` bytes.
fn trim_verify_entries(root: &Path, limit: u64, keep: u64) {
    let entries = verify_entries(root);
    let size = |path: &Path| path.join("input.mkv").metadata().map(|meta| meta.len()).unwrap_or(0);
    let mut total: u64 = entries.iter().map(|(path, _)| size(path)).sum();
    for (path, entry) in &entries {
        if total <= limit {
            break;
        }
        if entry.job_id == keep {
            continue;
        }
        total = total.saturating_sub(size(path));
        let _ = std::fs::remove_dir_all(path);
    }
}

// Starts the worker. It does nothing until `caramel_research` is on and the queue is empty.
pub fn spawn() {
    tokio::spawn(run());
}

#[cfg(test)]
mod tests {
    use super::*;

    // An encode's decisions and input become an entry; the oldest entries go once the inputs are
    // over the cap, never the one just kept; an encode with no decisions keeps nothing.
    #[test]
    fn caramel_encodes_are_kept_for_verification_under_a_cap() {
        let root = std::env::temp_dir().join(format!("pandora-verify-{}-{}", std::process::id(), unix_now()));
        let job = root.join("job");
        std::fs::create_dir_all(&job).unwrap();
        let input = job.join("input.mkv");
        std::fs::write(&input, vec![0u8; 100]).unwrap();
        let decisions = job.join("output.mp4.caramel-decisions");
        std::fs::write(&decisions, "#caramel-decisions model=m anchor=17 fps=23.976\n0|48|17|21|21||21/41.5/41|44/-3|detail|4.000||0\n48|20|17|17|17|||||0.100||0\n").unwrap();
        let verify = root.join("verify");
        for (id, now) in [(1u64, 10u64), (2, 20), (3, 30)] {
            let entry = keep_entry(&verify, id, &decisions, &input, "torrent:abc", now).expect("kept");
            assert_eq!((entry.scenes, entry.model.as_str()), (2, "m"));
            assert!(entry.series.starts_with("verify-"));
        }
        assert!(verify.join("3").join("input.mkv").exists());
        assert!(pnx264::scenes::EncodeDecisions::read(&verify.join("3").join("decisions")).is_some());
        // Every release of one source is one series.
        let series: Vec<String> = verify_entries(&verify).into_iter().map(|(_, entry)| entry.series).collect();
        assert!(series.windows(2).all(|pair| pair[0] == pair[1]));
        trim_verify_entries(&verify, 150, 1);
        let left: Vec<u64> = verify_entries(&verify).into_iter().map(|(_, entry)| entry.job_id).collect();
        assert_eq!(left, vec![1]);
        assert!(keep_entry(&verify, 4, &job.join("missing"), &input, "x", 40).is_none());
        assert!(!verify.join("4").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect()
    }

    #[test]
    fn research_is_off_until_it_is_turned_on() {
        let off = research_config(&env(&[]));
        assert_eq!(off, ResearchConfig { enabled: false, feed: None, scenes: 40, retrain: 200, preset: "standard".to_string(), crfs: Vec::new() });
        assert!(!research_config(&env(&[(CARAMEL_RESEARCH, "off")])).enabled);
        let on = research_config(&env(&[
            (CARAMEL_RESEARCH, " On "),
            (CARAMEL_RESEARCH_FEED, " https://example.org/?page=rss "),
            (CARAMEL_RESEARCH_SCENES, "9000"),
            (CARAMEL_RESEARCH_RETRAIN, "nonsense"),
            (CARAMEL_RESEARCH_PRESET, "Quality"),
            (CARAMEL_RESEARCH_CRFS, "31, 11 15,19,x,99,15"),
        ]));
        assert_eq!(on, ResearchConfig { enabled: true, feed: Some("https://example.org/?page=rss".to_string()), scenes: 400, retrain: 200, preset: "quality".to_string(), crfs: vec![11.0, 15.0, 19.0, 31.0] });
        assert_eq!(research_config(&env(&[(CARAMEL_RESEARCH_FEED, "  ")])).feed, None);
    }

    fn candidate(name: &str) -> Candidate {
        Candidate { path: PathBuf::from(name), source_id: name.to_string(), series: series_key(name), fetched: false }
    }

    #[test]
    fn a_series_is_measured_twice_and_then_left_alone() {
        let candidates = [candidate("[G] Show - 01 (1080p).mkv"), candidate("[G] Show - 02 (1080p).mkv"), candidate("[G] Show - 03 (1080p).mkv"), candidate("[G] Other - 01 (1080p).mkv")];
        let mut state = ResearchState::default();
        let mut dataset: Vec<(String, String)> = Vec::new();
        assert_eq!(pick(&candidates, &state, &dataset).unwrap().source_id, "[G] Show - 01 (1080p).mkv");
        state.sources.insert("[G] Show - 01 (1080p).mkv".to_string(), SourceRecord { series: "show".to_string(), finished_unix: 1, outcome: "measured".to_string() });
        assert_eq!(pick(&candidates, &state, &dataset).unwrap().source_id, "[G] Show - 02 (1080p).mkv");
        // The second episode was started and stopped for the queue: it is in the dataset and not
        // in the state, and is carried on with rather than counted against its series.
        dataset.push(("[G] Show - 02 (1080p).mkv".to_string(), "show".to_string()));
        assert_eq!(pick(&candidates, &state, &dataset).unwrap().source_id, "[G] Show - 02 (1080p).mkv");
        state.sources.insert("[G] Show - 02 (1080p).mkv".to_string(), SourceRecord { series: "show".to_string(), finished_unix: 2, outcome: "measured".to_string() });
        // Two of one show are enough: the third is passed over for another series.
        assert_eq!(pick(&candidates, &state, &dataset).unwrap().source_id, "[G] Other - 01 (1080p).mkv");
        state.sources.insert("[G] Other - 01 (1080p).mkv".to_string(), SourceRecord::default());
        assert_eq!(pick(&candidates, &state, &dataset), None);
    }

    #[test]
    fn a_feed_item_is_one_seeded_full_hd_episode_of_a_new_series() {
        let item = |key: &str, title: &str, bytes: u64, seeders: u32| FeedItem {
            key: key.to_string(),
            title: title.to_string(),
            link: format!("https://example.org/download/{key}.torrent"),
            size_bytes: Some(bytes),
            seeders: Some(seeders),
        };
        let gib = 1u64 << 30;
        let items = [
            item("a", "[G] Batch Show (01-12) (1080p)", 14 * gib, 90),
            item("b", "[G] Small Show - 03 (720p)", gib / 2, 90),
            item("c", "[G] Dead Show - 03 (1080p)", gib, 1),
            item("d", "[G] Known Show - 09 (1080p)", gib, 90),
            item("e", "[G] New Show - 03 (1080p)", gib, 90),
            item("f", "[G] Newer Show - 01 (1080p)", gib, 90),
        ];
        let mut state = ResearchState::default();
        let dataset = vec![("[G] Known Show - 07 (1080p).mkv".to_string(), "known show".to_string()), ("[G] Known Show - 08 (1080p).mkv".to_string(), "known show".to_string())];
        assert_eq!(pick_feed_item(&items, &state, &dataset).unwrap().key, "e");
        state.feed_seen.push("e".to_string());
        assert_eq!(pick_feed_item(&items, &state, &dataset).unwrap().key, "f");
        state.feed_seen.push("f".to_string());
        assert_eq!(pick_feed_item(&items, &state, &dataset), None);
        // A magnet, or a link that is not a .torrent over https, is not fetched.
        let odd = FeedItem { link: "magnet:?xt=urn:btih:0".to_string(), ..item("g", "[G] Odd Show - 01 (1080p)", gib, 90) };
        assert_eq!(pick_feed_item(&[odd], &ResearchState::default(), &dataset), None);
    }

    #[test]
    fn local_sources_are_the_operators_then_fetched_then_cached() {
        let root = std::env::temp_dir().join(format!("pandora-research-{}-{}", std::process::id(), unix_now()));
        let (sources, cache) = (root.join("sources"), root.join("cache"));
        let big = vec![0u8; (FETCH_BYTES.0 / 4) as usize];
        let put = |path: PathBuf, bytes: &[u8]| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        };
        put(sources.join("fetched").join("k1").join("[G] Fetched - 01 (1080p).mkv"), &big);
        put(sources.join("[G] Mine - 05 (1080p).mkv"), &big);
        put(sources.join("notes.txt"), b"not a video");
        put(sources.join("tiny.mkv"), b"too small to be an episode");
        put(sources.join(".hidden").join("[G] Hidden - 01.mkv"), &big);
        // A queued release is the queue's to measure, with the series it was queued as.
        put(sources.join("requested").join("0123456789ab").join("[G] Asked - 01 (1080p).mkv"), &big);
        // The same input under two keys, and a longer one.
        put(cache.join("aaaa").join("input.mkv"), &big);
        put(cache.join("bbbb").join("input.mkv"), &big);
        put(cache.join("cccc").join("input.mkv"), &[big.as_slice(), b"more"].concat());
        let found = local_candidates(&sources, &cache);
        std::fs::remove_dir_all(&root).unwrap();
        let ids: Vec<&str> = found.iter().map(|candidate| candidate.source_id.as_str()).collect();
        let (short, long) = (format!("cached-input-{}", big.len()), format!("cached-input-{}", big.len() + 4));
        assert_eq!(ids, ["[G] Mine - 05 (1080p).mkv", "[G] Fetched - 01 (1080p).mkv", short.as_str(), long.as_str()]);
        assert_eq!(found.iter().map(|candidate| candidate.fetched).collect::<Vec<_>>(), [false, true, false, false]);
        assert_eq!((found[0].series.as_str(), found[2].series.as_str()), ("mine", short.as_str()));
    }

    #[test]
    fn names_become_file_names_and_summaries_are_found() {
        let name = slug("[G] Some Show! - 03 (1080p) [ABCD].mkv");
        assert!(name.starts_with("g-some-show-03-1080p-abcd-mkv-") && name.len() <= 70, "{name}");
        assert_ne!(slug("a/b"), slug("a?b"));
        let output = "noise\n{\"measured\":12,\"cancelled\":false}\n\n";
        assert_eq!(summary_json(output).unwrap()["measured"], 12);
        assert_eq!(summary_json("nothing here\n[1,2]\n"), None);
    }

    #[test]
    fn queued_releases_are_named_and_checked() {
        assert_eq!(clean_series("  Ranma 1/2   2024 ").as_deref(), Some("ranma 1/2 2024"));
        assert_eq!(clean_series("   "), None);
        assert_eq!(clean_series(&"x".repeat(81)), None);
        assert!(is_request_id("0123456789ab"));
        assert!(!is_request_id("../../etc/xx"));
        assert!(!is_request_id("0123456789AB"));
        let requests = vec![
            SourceRequest { status: "queued".to_string(), ..Default::default() },
            SourceRequest { status: "queued".to_string(), ..Default::default() },
            SourceRequest { status: "measured".to_string(), ..Default::default() },
        ];
        assert_eq!(request_counts(&requests), json!({ "measured": 1, "queued": 2 }));
        // An entry from before a field existed still reads.
        let old: SourceRequest = serde_json::from_str(r#"{"id":"0123456789ab","link":"magnet:?xt=urn:btih:0","series":"x","status":"queued"}"#).unwrap();
        assert_eq!((old.file_index, old.attempts), (None, 0));
    }

    #[test]
    fn state_reads_back_and_tolerates_older_files() {
        let mut state = ResearchState { trained_scenes: 400, feed_seen: vec!["a".to_string()], ..Default::default() };
        state.sources.insert("x.mkv".to_string(), SourceRecord { series: "x".to_string(), finished_unix: 5, outcome: "measured".to_string() });
        let text = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<ResearchState>(&text).unwrap(), state);
        assert_eq!(serde_json::from_str::<ResearchState>("{}").unwrap(), ResearchState::default());
    }
}
