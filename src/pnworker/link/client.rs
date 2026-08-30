use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::mpsc::Sender;

use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{
    LINK_COORDINATOR_URL, LINK_MAX_JOBS, LINK_NODE_NAME, LINK_NODE_TOKEN, PANDORA_MODE,
};
use crate::lib::p2p::nyaaise::TorrentType;
use crate::pnworker::core::{HalfJob, Job, JobClass, JobType, Stage};
use crate::pnworker::frontend::Frontend;
use crate::pnworker::link::assets::{self, AssetKind, AssetManifest};
use crate::pnworker::link::spec::{
    LeaseControl, LeaseRenew, LeaseResult, LinkJobSpec, LinkLogChunk, LinkOutcome, LinkPayload,
    LinkReport, NodeRegister, NodeRegistered, job_type_from_name, source_from_wire, stage_name,
};
use crate::pnworker::messages::MessagePayload;
use crate::pnworker::server_effects::preset_from_name;

// The node half of the link. It owns no jobs of its own: it leases one from the coordinator, hands
// it to this process's ordinary `pn_worker`, forwards everything that worker says back up, and
// reports the outcome. Nothing below knows what a job *is* beyond the spec — that is the point of
// running the real worker runtime here rather than a special remote-execution path.

// A node has no inbound surface at all, so every request in this file is outbound. The lease poll
// is a long poll, which is why its timeout is generous where the others are not.
const LEASE_POLL_TIMEOUT_SECS: u64 = 45;
const REQUEST_TIMEOUT_SECS: u64 = 30;
const REGISTER_RETRY_SECS: u64 = 15;
// A single font or intro variant, which is a much larger body than any control message.
const ASSET_TIMEOUT_SECS: u64 = 300;

// Installing fonts into the OS font path and refreshing fontconfig lives in `src/helpers`, which
// is compiled into the `pndc` binary and not into this library, so the node is handed the call
// rather than reaching for it. It runs after a reconcile that added fonts, because libass resolves
// through system fontconfig — a font sitting in `DB/fontconfig` that has not been installed is a
// font that is still not found.
pub type FontRefresh = fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

#[derive(Clone, Copy)]
struct ActiveLease {
    job_id: u64,
    return_output: bool,
}

pub struct LinkConfig {
    pub coordinator: String,
    pub token: String,
    pub node: String,
    pub max_jobs: u32,
}

pub fn is_mini() -> bool {
    static MINI: OnceLock<bool> = OnceLock::new();
    *MINI.get_or_init(|| {
        if std::env::args().any(|arg| arg == "--mini") {
            return true;
        }
        get_pandora_env()
            .get(PANDORA_MODE)
            .map(|value| value.trim().eq_ignore_ascii_case("mini"))
            .unwrap_or(false)
    })
}

pub fn load_config() -> Result<LinkConfig, String> {
    let env = get_pandora_env();
    let coordinator = env
        .get(LINK_COORDINATOR_URL)
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{LINK_COORDINATOR_URL} is not set"))?;
    let token = env
        .get(LINK_NODE_TOKEN)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{LINK_NODE_TOKEN} is not set"))?;
    let node = env
        .get(LINK_NODE_NAME)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{LINK_NODE_NAME} is not set"))?;
    let max_jobs = env
        .get(LINK_MAX_JOBS)
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1);
    Ok(LinkConfig {
        coordinator,
        token,
        node,
        max_jobs,
    })
}

// Which libx264 this build encodes with. It is the whole of what has to match between two
// machines: a node with a different Rust compiler or libc produces identical frames, while one
// with a different x264 makes different rate decisions at the same CRF. Hashing the pnmpeg binary
// instead — the obvious thing, and what this used to do — refuses builds that are genuinely
// equivalent, which pushes an operator into disabling the check altogether.
pub fn encoder_identity() -> String {
    pnx264::identity()
}

fn ffmpeg_version() -> String {
    let binary = crate::lib::bin::resolve_runtime_binary("ffmpeg");
    let Ok(output) = std::process::Command::new(binary).arg("-version").output() else {
        return String::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

// Reports the local worker runtime has produced but not yet sent. `render` is the single place
// every user-visible effect passes through, so hooking it captures declines and cancellations as
// faithfully as it captures encode progress — none of which reach a `CommData` stream.
struct Pending {
    reports: Vec<LinkReport>,
    worker: String,
    stage: Option<Stage>,
    terminal: Option<Stage>,
}

fn pending() -> &'static Mutex<HashMap<u64, Pending>> {
    static PENDING: OnceLock<Mutex<HashMap<u64, Pending>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

fn leased() -> &'static Mutex<std::collections::HashSet<u64>> {
    static LEASED: OnceLock<Mutex<std::collections::HashSet<u64>>> = OnceLock::new();
    LEASED.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

// Called from `lifecycle::render` for every job on this process. On a coordinator it returns
// immediately; on a node it queues the payload for the next renew.
pub fn report(job_id: u64, payload: &MessagePayload, stage: Stage, worker: &str) {
    if !is_mini() {
        return;
    }
    if !leased().lock().is_ok_and(|set| set.contains(&job_id)) {
        return;
    }
    let wire = LinkPayload::from_payload(payload);
    let Ok(mut map) = pending().lock() else {
        return;
    };
    let entry = map.entry(job_id).or_insert_with(|| Pending {
        reports: Vec::new(),
        worker: String::new(),
        stage: None,
        terminal: None,
    });
    entry.worker = worker.to_string();
    entry.stage = Some(stage);
    // Where the node's involvement ends. `Probed` belongs here even though the job lives on: a
    // probe stops there locally too, waiting for a file to be selected, and holding the lease past
    // it would let the lease expire and have the coordinator re-run a probe that already answered.
    if matches!(
        stage,
        Stage::Uploaded | Stage::Failed | Stage::Declined | Stage::Cancelled | Stage::Probed
    ) {
        entry.terminal = Some(stage);
    }
    // Encode progress arrives every few seconds and only the newest tick means anything, so a
    // repeat of the same message id replaces its predecessor instead of growing the buffer. A
    // different id is a different event and is always kept.
    if let Some(last) = entry.reports.last_mut() {
        if last.payload.id == wire.id && last.stage.as_deref() == Some(stage_name(stage)).as_deref()
        {
            *last = LinkReport {
                payload: wire,
                stage: Some(stage_name(stage)),
            };
            return;
        }
    }
    entry.reports.push(LinkReport {
        payload: wire,
        stage: Some(stage_name(stage)),
    });
}

struct Drained {
    reports: Vec<LinkReport>,
    worker: String,
    stage: Option<Stage>,
    terminal: Option<Stage>,
}

fn take_reports(job_id: u64) -> Drained {
    let Ok(mut map) = pending().lock() else {
        return Drained { reports: Vec::new(), worker: String::new(), stage: None, terminal: None };
    };
    let Some(entry) = map.get_mut(&job_id) else {
        return Drained { reports: Vec::new(), worker: String::new(), stage: None, terminal: None };
    };
    Drained {
        reports: std::mem::take(&mut entry.reports),
        worker: entry.worker.clone(),
        stage: entry.stage,
        terminal: entry.terminal,
    }
}

// Jobs whose output has been handed back to the coordinator. The node's worker loop holds such a
// job at `Encoded` — its output is not this machine's to publish — and finishes it once this says
// the file is gone. It is the same shape of back-channel as `leased`, and for the same reason:
// the loop owns the queue and this task owns the link.
fn returned() -> &'static Mutex<std::collections::HashSet<u64>> {
    static RETURNED: OnceLock<Mutex<std::collections::HashSet<u64>>> = OnceLock::new();
    RETURNED.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

pub fn output_returned(job_id: u64) -> bool {
    returned().lock().is_ok_and(|set| set.contains(&job_id))
}

fn mark_returned(job_id: u64) {
    if let Ok(mut set) = returned().lock() {
        set.insert(job_id);
    }
}

fn forget(job_id: u64) {
    if let Ok(mut map) = pending().lock() {
        map.remove(&job_id);
    }
    if let Ok(mut set) = leased().lock() {
        set.remove(&job_id);
    }
    if let Ok(mut set) = returned().lock() {
        set.remove(&job_id);
    }
}

// Streams a finished encode back to the coordinator. This is the one large body the link carries,
// and only for HLS-only servers, where the alternative is a playback URL on a machine with no
// public hostname.
async fn put_output(
    client: &reqwest::Client,
    config: &LinkConfig,
    job_id: u64,
    path: &std::path::Path,
) -> Result<(), String> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let length = file
        .metadata()
        .await
        .map(|meta| meta.len())
        .map_err(|e| e.to_string())?;
    if length == 0 {
        return Err(format!("{} is empty", path.display()));
    }
    let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
    let response = client
        .put(format!(
            "{}/api/v1/link/lease/{job_id}/output",
            config.coordinator
        ))
        .bearer_auth(&config.token)
        .header(reqwest::header::CONTENT_LENGTH, length)
        .header("x-pandora-node", &config.node)
        // Deliberately no timeout: this is a multi-gigabyte upload over whatever link the node has.
        .timeout(Duration::from_secs(u32::MAX as u64))
        .body(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "coordinator answered {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }
    Ok(())
}

async fn fetch_manifest(
    client: &reqwest::Client,
    config: &LinkConfig,
) -> Result<AssetManifest, String> {
    let response = client
        .get(format!("{}/api/v1/link/assets/manifest", config.coordinator))
        .bearer_auth(&config.token)
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("coordinator answered {}", response.status()));
    }
    response.json::<AssetManifest>().await.map_err(|e| e.to_string())
}

async fn fetch_asset(
    client: &reqwest::Client,
    config: &LinkConfig,
    hash: &str,
) -> Result<Vec<u8>, String> {
    let response = client
        .get(format!("{}/api/v1/link/assets/{hash}", config.coordinator))
        .bearer_auth(&config.token)
        .timeout(Duration::from_secs(ASSET_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("coordinator answered {}", response.status()));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|e| e.to_string())
}

// Brings this node's font and intro corpus up to the coordinator's, and records the revision it
// reached. Only what is missing is fetched, compared by content — a node that already holds a font
// under a different mtime does not download it again.
async fn reconcile_assets(
    client: &reqwest::Client,
    config: &LinkConfig,
    refresh_fonts: FontRefresh,
) -> Result<String, String> {
    let manifest = fetch_manifest(client, config).await?;
    let missing = assets::missing(&manifest);
    if missing.is_empty() {
        assets::record_revision(&manifest.revision);
        return Ok(manifest.revision);
    }
    println!(
        "[link] syncing {} asset(s) for revision {}",
        missing.len(),
        &manifest.revision[..manifest.revision.len().min(12)]
    );
    let mut installed_fonts = false;
    for entry in &missing {
        let bytes = fetch_asset(client, config, &entry.hash).await?;
        assets::write_asset(entry, &bytes)?;
        installed_fonts |= entry.kind == AssetKind::Font;
    }
    // libass reads the OS font path, not `DB/fontconfig`, so a synced font is not a usable font
    // until this has run.
    if installed_fonts {
        refresh_fonts().await;
    }
    assets::record_revision(&manifest.revision);
    println!("[link] asset sync complete ({} file(s))", missing.len());
    Ok(manifest.revision)
}

pub async fn run(tx: Sender<JobClass>, refresh_fonts: FontRefresh) {
    let config = match load_config() {
        Ok(config) => config,
        Err(reason) => {
            eprintln!("[link] mini mode is enabled but not configured: {reason}");
            return;
        }
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(LEASE_POLL_TIMEOUT_SECS))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            eprintln!("[link] could not build an HTTP client: {e}");
            return;
        }
    };
    println!(
        "[link] node {} linking to {} (max {} concurrent job(s))",
        config.node, config.coordinator, config.max_jobs
    );

    let renew_secs;
    let registered_revision;
    loop {
        match register(&client, &config).await {
            Ok(registered) if registered.accepted => {
                renew_secs = registered.renew_secs.max(1);
                println!("[link] node {} registered", config.node);
                // Remembered so a sync that fails here is retried between polls. Without it a node
                // whose first sync failed would sit unsynced until a job happened to arrive, and
                // then decline it — correct, but it would never recover on its own.
                registered_revision = registered.assets_revision.clone();
                // Sync before the first poll rather than after, so the common case is a node that
                // is already current by the time it is offered anything.
                if let Err(e) = reconcile_assets(&client, &config, refresh_fonts).await {
                    eprintln!("[link] asset sync failed: {e}");
                }
                break;
            }
            Ok(registered) => {
                eprintln!(
                    "[link] coordinator refused this node: {}",
                    registered.reason.unwrap_or_else(|| "no reason given".to_string())
                );
            }
            Err(e) => eprintln!("[link] registration failed: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(REGISTER_RETRY_SECS)).await;
    }

    let mut active: Vec<ActiveLease> = Vec::new();
    let mut draining = false;
    let mut wanted_revision = registered_revision;
    // How much of each of a job's logs has been shipped. Advanced only once the renew carrying a
    // chunk succeeded, so a failed request costs a repeat rather than a hole in the transcript.
    let mut log_offsets: HashMap<u64, HashMap<String, u64>> = HashMap::new();
    loop {
        let mut finished: Vec<u64> = Vec::new();
        for lease in active.clone() {
            let job_id = lease.job_id;
            let Drained { reports, worker, stage, terminal } = take_reports(job_id);
            if let Some(stage) = terminal {
                // The lease ends with this result, and with it the only channel these logs have.
                // Whatever the tools wrote in their last seconds is exactly the part worth reading.
                let offsets = log_offsets.entry(job_id).or_default();
                flush_logs(&client, &config, job_id, offsets).await;
                let outcome = match stage {
                    Stage::Uploaded => LinkOutcome::Uploaded,
                    Stage::Probed => LinkOutcome::Probed,
                    Stage::Cancelled => LinkOutcome::Cancelled,
                    Stage::Declined => LinkOutcome::Declined,
                    _ => LinkOutcome::Failed,
                };
                let result = LeaseResult {
                    node: config.node.clone(),
                    outcome,
                    reason: None,
                    reports,
                    warnings: Vec::new(),
                };
                if let Err(e) = send_result(&client, &config, job_id, &result).await {
                    // Keep the lease so the next pass tries again; the coordinator's watchdog is
                    // the backstop if this node cannot reach it at all.
                    eprintln!("[link] job {job_id} result could not be delivered: {e}");
                    continue;
                }
                println!("[link] job {job_id} finished as {:?}", outcome);
                forget(job_id);
                log_offsets.remove(&job_id);
                finished.push(job_id);
                continue;
            }
            // An HLS-only job stops here: the encode is done and the output is the coordinator's
            // to publish. Send it, release the local job, then report — in that order, because a
            // report that never lands only costs a requeue, while a work directory wiped before
            // the file was sent costs the encode.
            if lease.return_output && stage == Some(Stage::Encoded) && !output_returned(job_id) {
                let path = std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join("DB")
                    .join("work")
                    .join(job_id.to_string())
                    .join("work")
                    .join("output.mp4");
                println!("[link] job {job_id} encoded; returning its output to the coordinator");
                match put_output(&client, &config, job_id, &path).await {
                    Ok(()) => {
                        mark_returned(job_id);
                        let result = LeaseResult {
                            node: config.node.clone(),
                            outcome: LinkOutcome::Returned,
                            reason: None,
                            reports,
                            warnings: Vec::new(),
                        };
                        if let Err(e) = send_result(&client, &config, job_id, &result).await {
                            eprintln!("[link] job {job_id} return could not be reported: {e}");
                        }
                        println!("[link] job {job_id} output returned");
                        forget(job_id);
                        log_offsets.remove(&job_id);
                        finished.push(job_id);
                    }
                    Err(e) => eprintln!("[link] job {job_id} output could not be returned: {e}"),
                }
                continue;
            }
            let offsets = log_offsets.entry(job_id).or_default();
            let (chunks, advanced) = crate::pnworker::link::logs::collect(job_id, offsets).await;
            match send_renew(&client, &config, job_id, reports, worker, chunks).await {
                Ok(control) => {
                    *offsets = advanced;
                    draining = control.drain;
                    if !control.assets_revision.is_empty() {
                        wanted_revision = control.assets_revision.clone();
                    }
                    if control.abandon {
                        eprintln!("[link] job {job_id} was reclaimed by the coordinator; dropping it");
                        cancel_locally(&tx, job_id).await;
                        forget(job_id);
                        log_offsets.remove(&job_id);
                        finished.push(job_id);
                    } else if control.cancel {
                        cancel_locally(&tx, job_id).await;
                    }
                }
                Err(e) => eprintln!("[link] job {job_id} renew failed: {e}"),
            }
        }
        active.retain(|lease| !finished.contains(&lease.job_id));

        // A font added on the coordinator reaches a working node here, between jobs, rather than
        // waiting for it to restart. Syncing before the poll is what keeps the refusal below rare.
        if !wanted_revision.is_empty() && assets::local_revision().as_deref() != Some(&wanted_revision)
        {
            if let Err(e) = reconcile_assets(&client, &config, refresh_fonts).await {
                eprintln!("[link] asset sync failed: {e}");
            }
        }

        if !draining && (active.len() as u32) < config.max_jobs {
            match poll_lease(&client, &config).await {
                Ok(Some(spec)) => {
                    let return_output = spec.return_output;
                    match accept(&tx, &client, &config, refresh_fonts, spec).await {
                    Ok(job_id) => {
                        println!("[link] job {job_id} leased");
                        active.push(ActiveLease { job_id, return_output });
                    }
                    Err((job_id, reason)) => {
                        eprintln!("[link] leased job {job_id} was declined: {reason}");
                        let result = LeaseResult {
                            node: config.node.clone(),
                            outcome: LinkOutcome::Declined,
                            reason: Some(reason),
                            reports: Vec::new(),
                            warnings: Vec::new(),
                        };
                        send_result(&client, &config, job_id, &result).await.ok();
                    }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("[link] lease poll failed: {e}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(renew_secs.min(30))).await;
    }
}

async fn register(
    client: &reqwest::Client,
    config: &LinkConfig,
) -> Result<NodeRegistered, String> {
    let body = NodeRegister {
        node: config.node.clone(),
        pandora_version: env!("CARGO_PKG_VERSION").to_string(),
        encoder_identity: encoder_identity(),
        ffmpeg_version: ffmpeg_version(),
        threads: std::thread::available_parallelism()
            .map(|value| value.get() as u32)
            .unwrap_or(1),
        max_jobs: config.max_jobs,
        presets: Vec::new(),
    };
    let response = client
        .post(format!("{}/api/v1/link/register", config.coordinator))
        .bearer_auth(&config.token)
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "coordinator answered {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }
    response.json::<NodeRegistered>().await.map_err(|e| e.to_string())
}

async fn poll_lease(
    client: &reqwest::Client,
    config: &LinkConfig,
) -> Result<Option<LinkJobSpec>, String> {
    let response = client
        .get(format!(
            "{}/api/v1/link/lease?node={}",
            config.coordinator, config.node
        ))
        .bearer_auth(&config.token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!("coordinator answered {}", response.status()));
    }
    response
        .json::<LinkJobSpec>()
        .await
        .map(Some)
        .map_err(|e| e.to_string())
}

async fn send_renew(
    client: &reqwest::Client,
    config: &LinkConfig,
    job_id: u64,
    reports: Vec<LinkReport>,
    worker: String,
    logs: Vec<LinkLogChunk>,
) -> Result<LeaseControl, String> {
    let body = LeaseRenew {
        node: config.node.clone(),
        worker,
        reports,
        logs,
    };
    let response = client
        .post(format!(
            "{}/api/v1/link/lease/{job_id}/renew",
            config.coordinator
        ))
        .bearer_auth(&config.token)
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("coordinator answered {}", response.status()));
    }
    response.json::<LeaseControl>().await.map_err(|e| e.to_string())
}

// Ships everything a job's logs still hold, in as many renews as it takes. Bounded, because a log
// growing faster than it can be sent must not hold the lease open forever — the cap is far above
// what an encoder transcript reaches.
async fn flush_logs(
    client: &reqwest::Client,
    config: &LinkConfig,
    job_id: u64,
    offsets: &mut HashMap<String, u64>,
) {
    for _ in 0..32 {
        let (chunks, advanced) = crate::pnworker::link::logs::collect(job_id, offsets).await;
        if chunks.is_empty() {
            return;
        }
        match send_renew(client, config, job_id, Vec::new(), String::new(), chunks).await {
            Ok(_) => *offsets = advanced,
            Err(e) => {
                eprintln!("[link] job {job_id} final logs could not be shipped: {e}");
                return;
            }
        }
    }
}

async fn send_result(
    client: &reqwest::Client,
    config: &LinkConfig,
    job_id: u64,
    result: &LeaseResult,
) -> Result<(), String> {
    let response = client
        .post(format!(
            "{}/api/v1/link/lease/{job_id}/result",
            config.coordinator
        ))
        .bearer_auth(&config.token)
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .json(result)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("coordinator answered {}", response.status()));
    }
    Ok(())
}

// A cancel reaches the node as a flag on a renew response, and from there takes the ordinary local
// path: the same `HalfJob` a ❌ reaction produces. `any_author` is set because the job's author is
// a Discord user this machine has never heard of.
async fn cancel_locally(tx: &Sender<JobClass>, job_id: u64) {
    let mut halfjob = HalfJob::new_cancel(0, 0, job_id);
    halfjob.any_author = true;
    halfjob.frontend = Frontend::None;
    tx.send(JobClass::HalfJob(halfjob)).await.ok();
}

// Turns a leased spec into an ordinary local job. Anything it cannot honour — a preset this build
// does not know, a source kind it cannot classify — is declined rather than guessed at, because a
// node quietly encoding with the wrong preset is worse than a coordinator re-running the job.
async fn accept(
    tx: &Sender<JobClass>,
    client: &reqwest::Client,
    config: &LinkConfig,
    refresh_fonts: FontRefresh,
    spec: LinkJobSpec,
) -> Result<u64, (u64, String)> {
    let job_id = spec.job_id.parse::<u64>().unwrap_or(0);
    let decline = |reason: String| Err((job_id, reason));
    if job_id == 0 {
        return decline("job id is not a number".to_string());
    }
    // The corpus check, and the reason this exists at all: a missing font does not fail, it
    // substitutes, and a release goes out in the wrong typeface with nothing to show for it. A
    // node that cannot prove it holds what the job was built against refuses the work instead.
    if !spec.assets_revision.is_empty()
        && assets::local_revision().as_deref() != Some(spec.assets_revision.as_str())
    {
        if let Err(e) = reconcile_assets(client, config, refresh_fonts).await {
            return decline(format!("assets could not be synced: {e}"));
        }
        if assets::local_revision().as_deref() != Some(spec.assets_revision.as_str()) {
            return decline(format!(
                "asset revision {} is not what this node holds",
                &spec.assets_revision[..spec.assets_revision.len().min(12)]
            ));
        }
    }
    let Some(job_type) = job_type_from_name(&spec.job_type) else {
        return decline(format!("unsupported job type {}", spec.job_type));
    };
    let Some(source) = source_from_wire(&spec.source_kind, &spec.source) else {
        return decline(format!("unsupported source kind {}", spec.source_kind));
    };
    // The concat folder inside a preset is a path on the coordinator. What arrived is the group's
    // name; the folder is wherever this node materialised it. An empty group would hand pnmpeg a
    // folder with no variants and quietly produce a release with no intro, which is the same class
    // of failure as a substituted font.
    let intro_candidates = match spec.intro_group.as_deref() {
        None => None,
        Some(group) => {
            if !assets::intro_group_is_populated(group) {
                return decline(format!("intro group {group} synced no files"));
            }
            Some(assets::intro_dir(group).display().to_string())
        }
    };
    let Some(preset) = preset_from_name(&spec.preset, intro_candidates) else {
        return decline(format!("unsupported preset {}", spec.preset));
    };
    let attachment = match decode_base64(&spec.subtitle_b64) {
        Some(bytes) => bytes,
        None => return decline("subtitle is not valid base64".to_string()),
    };
    let watermark = match spec.watermark_b64.as_deref() {
        None => None,
        Some(encoded) => match decode_base64(encoded) {
            Some(bytes) => Some(bytes),
            None => return decline("watermark is not valid base64".to_string()),
        },
    };

    let mut job = Job::new_api(
        0,
        0,
        job_type,
        source,
        attachment,
        spec.lang.clone(),
        None,
    );
    // The coordinator's id is reused verbatim so the node's logs, this process's work directory and
    // the row the operator is watching all carry the same number.
    job.job_id = job_id;
    job.directory = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("DB")
        .join("work")
        .join(job_id.to_string());
    job.preset = preset;
    job.server_watermark = watermark;
    job.frontend = Frontend::None;
    job.display_link = spec.display_link.clone();
    job.probe_file_index = spec.file_index;
    // Carried so `queue_pancode_job` does not refuse the job for having no probe at all. Adopting
    // the probe's saved torrent will fail here — there is no probe job on this machine — and the
    // source link the spec carries is what the download falls back to.
    job.probe_job_id = spec.probe_job_id.as_deref().and_then(|id| id.parse::<u64>().ok());
    job.gdrive_folder_global = spec.gdrive_folder_global.clone();
    job.gdrive_folder_local = spec.gdrive_folder_local.clone();
    job.link_return_output = spec.return_output;
    // The originating guild, so Drive uploads land under its Lumiere profile — and its upload
    // policy, which travels with the job because this machine holds no `meta.pandora` for it.
    job.server_id = spec.server_id.as_deref().and_then(|id| id.parse::<u64>().ok());
    job.link_drive_only = Some(spec.drive_only);

    if let Ok(mut set) = leased().lock() {
        set.insert(job_id);
    }
    if tx.send(JobClass::Job(job)).await.is_err() {
        forget(job_id);
        return decline("this node's worker queue is closed".to_string());
    }
    Ok(job_id)
}

fn decode_base64(encoded: &str) -> Option<Vec<u8>> {
    let encoded = encoded.trim();
    if encoded.is_empty() {
        return Some(Vec::new());
    }
    let table = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(encoded.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in encoded.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        let value = table(byte)?;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

pub fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let value = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(value >> 18) as usize & 63] as char);
        out.push(TABLE[(value >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(value >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[value as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// Kept for the coordinator side, which builds a spec from a job it already holds.
pub fn source_kind_of(torrent: &TorrentType) -> String {
    torrent.get_arg()
}

// A job type this build cannot execute must never be leased in the first place; the coordinator
// asks the same question before it offers.
pub fn job_type_is_leasable(job_type: JobType) -> bool {
    matches!(
        job_type,
        JobType::Encode | JobType::Pancode | JobType::Backup | JobType::Probe
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_the_bytes_a_subtitle_travels_as() {
        for original in [
            b"".to_vec(),
            b"a".to_vec(),
            b"ab".to_vec(),
            b"abc".to_vec(),
            b"[Script Info]\nScriptType: v4.00+\n".to_vec(),
            (0u8..=255).collect::<Vec<u8>>(),
        ] {
            let encoded = encode_base64(&original);
            assert_eq!(decode_base64(&encoded), Some(original));
        }
    }

    #[test]
    fn base64_rejects_input_that_is_not_base64() {
        assert!(decode_base64("not valid!").is_none());
    }

    #[test]
    fn only_self_sourced_job_types_are_leasable() {
        assert!(job_type_is_leasable(JobType::Encode));
        assert!(job_type_is_leasable(JobType::Pancode));
        // A batch child is born already downloaded out of its parent's torrent, so it carries no
        // source of its own and cannot be handed to a node that fetches its own input.
        assert!(!job_type_is_leasable(JobType::Batch));
        assert!(!job_type_is_leasable(JobType::Studio));
        assert!(!job_type_is_leasable(JobType::Keycode));
    }
}
