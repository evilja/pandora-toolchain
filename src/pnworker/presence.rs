use std::sync::OnceLock;
use serenity::all::{ActivityData, Context, OnlineStatus};
use crate::libpnenv::standard::FLAVOR_PATH;
use crate::pnworker::core::{Job, Stage};

static DISCORD_CTX: OnceLock<Context> = OnceLock::new();

pub fn set_global_context(ctx: Context) {
    let _ = DISCORD_CTX.set(ctx);
}

pub fn global_context() -> Option<&'static Context> {
    DISCORD_CTX.get()
}

#[derive(Debug, Clone, Copy)]
pub enum Presence {
    Idle,
    QueueTotal(usize),
    Downloading { idx: usize, total: usize },
    Encoding    { idx: usize, total: usize },
    Uploading   { idx: usize, total: usize },
    Probing     { idx: usize, total: usize },
}

pub async fn change_presence_job(gateway: &Context, p: Presence) {
    let (text, status) = match p {
        Presence::Idle => (idle_presence_text().await, OnlineStatus::Online),
        Presence::QueueTotal(0) => (idle_presence_text().await, OnlineStatus::Online),
        Presence::QueueTotal(n) => (format!("{} jobs in queue (idle).", n), OnlineStatus::Online),
        Presence::Downloading { idx, total } => (format!("Downloading #{} of {} jobs", idx + 1, total), OnlineStatus::DoNotDisturb),
        Presence::Encoding    { idx, total } => (format!("Encoding #{} of {} jobs", idx + 1, total), OnlineStatus::DoNotDisturb),
        Presence::Uploading   { idx, total } => (format!("Uploading #{} of {} jobs", idx + 1, total), OnlineStatus::DoNotDisturb),
        Presence::Probing     { idx, total } => (format!("Probing #{} of {} jobs", idx + 1, total), OnlineStatus::DoNotDisturb),
    };
    gateway.set_presence(Some(ActivityData::custom(text)), status);
}

pub async fn idle_flavors() -> Vec<String> {
    let contents = match tokio::fs::read_to_string(FLAVOR_PATH).await {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    contents.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

async fn idle_presence_text() -> String {
    let flavors = idle_flavors().await;
    if flavors.is_empty() {
        return "No jobs in queue.".to_string();
    }
    let mut bytes = [0u8; 8];
    let idx = if getrandom::getrandom(&mut bytes).is_ok() {
        (u64::from_ne_bytes(bytes) as usize) % flavors.len()
    } else {
        0
    };
    flavors[idx].clone()
}

pub fn presence_from_queue(queue: &[Job]) -> Presence {
    if queue.is_empty() {
        return Presence::Idle;
    }
    let total = queue.len();
    let priority = |s: Stage| -> u8 {
        match s {
            Stage::Uploading   => 5,
            Stage::Encoding    => 4,
            Stage::Downloading => 3,
            Stage::Probing     => 2,
            Stage::Probed      => 1,
            _                  => 0,
        }
    };
    let mut best_idx: usize = 0;
    let mut best_stage: Stage = Stage::Queued;
    let mut best_pri: u8 = 0;
    for (idx, job) in queue.iter().enumerate() {
        let p = priority(job.ready);
        if p > best_pri {
            best_pri = p;
            best_stage = job.ready;
            best_idx = idx;
        }
    }
    match best_stage {
        Stage::Uploading   => Presence::Uploading   { idx: best_idx, total },
        Stage::Encoding    => Presence::Encoding    { idx: best_idx, total },
        Stage::Downloading => Presence::Downloading { idx: best_idx, total },
        Stage::Probing     => Presence::Probing     { idx: best_idx, total },
        _ => Presence::QueueTotal(total),
    }
}
