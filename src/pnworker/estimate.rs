use std::collections::HashMap;

use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant};

use crate::lib::db::core::JobDb;
use crate::lib::mpeg::probe::ffprobe_frame;
use crate::pnworker::cache::input_cache_keys;
use crate::pnworker::core::{Job, JobType, Preset, Stage};
use crate::pnworker::lifecycle::render;
use crate::pnworker::messages::{MessagePayload, QUEUE_POSITION};

const RENDER_INTERVAL: Duration = Duration::from_secs(30);
const UNKNOWN_JOB_ESTIMATE_SECS: u64 = 20 * 60;

pub(crate) struct QueueEstimator {
    frames_by_job: HashMap<u64, Option<u64>>,
    frames_by_key: HashMap<String, Option<u64>>,
    job_keys: HashMap<u64, String>,
    in_flight: Option<(u64, String, JoinHandle<Option<u64>>)>,
    last_render: HashMap<u64, (usize, Instant)>,
}

impl QueueEstimator {
    pub(crate) fn new() -> Self {
        Self {
            frames_by_job: HashMap::new(),
            frames_by_key: HashMap::new(),
            job_keys: HashMap::new(),
            in_flight: None,
            last_render: HashMap::new(),
        }
    }

    pub(crate) async fn tick(&mut self, db: &JobDb, queue: &mut Vec<Job>) {
        self.collect_probe_result().await;
        self.start_probe(queue);
        let waiting: Vec<u64> = queue
            .iter()
            .filter(|job| render_queue_estimate_for_job(job))
            .map(|job| job.job_id)
            .collect();
        for job_id in waiting {
            let Some(pos) = queue.iter().position(|job| job.job_id == job_id) else {
                continue;
            };
            let position = queue_position(pos, queue);
            let now = Instant::now();
            let should_render = self
                .last_render
                .get(&job_id)
                .map(|(last_position, last_time)| {
                    *last_position != position || now.duration_since(*last_time) >= RENDER_INTERVAL
                })
                .unwrap_or(true);
            if !should_render {
                continue;
            }
            let eta_secs = estimate_wait_secs(pos, queue, self);
            let eta_text = eta_secs
                .map(format_eta)
                .unwrap_or_else(|| "?".to_string());
            let progress = serde_json::json!({
                "type": "queue",
                "position": position,
                "eta_secs": eta_secs,
            });
            db.update_progress(job_id, &progress.to_string()).await.ok();
            render(
                &mut queue[pos],
                MessagePayload::Progress(QUEUE_POSITION, vec![position.to_string(), eta_text]),
            )
            .await;
            self.last_render.insert(job_id, (position, now));
        }
        self.last_render
            .retain(|job_id, _| queue.iter().any(|job| job.job_id == *job_id));
    }

    async fn collect_probe_result(&mut self) {
        let Some((job_id, key, handle)) = self.in_flight.take() else {
            return;
        };
        if !handle.is_finished() {
            self.in_flight = Some((job_id, key, handle));
            return;
        }
        let result = handle.await.ok().flatten();
        self.frames_by_job.insert(job_id, result);
        self.frames_by_key.insert(key, result);
    }

    fn start_probe(&mut self, queue: &[Job]) {
        if self.in_flight.is_some() {
            return;
        }
        for job in queue
            .iter()
            .filter(|job| waiting_encode_job(job) || active_encode_job(job))
        {
            if self.frames_for_job(job).is_some() {
                continue;
            }
            let key = input_cache_keys(job)
                .into_iter()
                .next()
                .unwrap_or_else(|| format!("job:{}", job.job_id));
            self.job_keys.insert(job.job_id, key.clone());
            if self.frames_by_key.contains_key(&key) || self.frames_by_job.contains_key(&job.job_id) {
                continue;
            }
            let path = job
                .directory
                .join("contents")
                .join("torrent")
                .join("input.mkv")
                .display()
                .to_string();
            let job_id = job.job_id;
            self.in_flight = Some((job_id, key, tokio::task::spawn_blocking(move || {
                ffprobe_frame(&path)
            })));
            return;
        }
    }

    fn frames_for_job(&self, job: &Job) -> Option<Option<u64>> {
        if let Some(frames) = self.frames_by_job.get(&job.job_id) {
            return Some(*frames);
        }
        let key = self.job_keys.get(&job.job_id)?;
        self.frames_by_key.get(key).copied()
    }
}

fn waiting_encode_job(job: &Job) -> bool {
    job.forward_parent.is_none()
        && matches!(job.job_type, JobType::Encode | JobType::Pancode)
        && job.ready == Stage::Downloaded
}

fn duplicate_cache_waiting_job(job: &Job) -> bool {
    job.forward_parent.is_none()
        && matches!(job.job_type, JobType::Encode | JobType::Pancode)
        && job.ready == Stage::Downloading
        && job.duplicate_source.is_some()
}

fn render_queue_estimate_for_job(job: &Job) -> bool {
    waiting_encode_job(job) || duplicate_cache_waiting_job(job)
}

fn active_encode_job(job: &Job) -> bool {
    job.forward_parent.is_none()
        && matches!(job.job_type, JobType::Encode | JobType::Pancode | JobType::Keycode)
        && job.ready == Stage::Encoding
}

pub(crate) fn queue_position(pos: usize, queue: &[Job]) -> usize {
    1 + queue
        .iter()
        .take(pos)
        .filter(|job| render_queue_estimate_for_job(job) || active_encode_job(job))
        .count()
}

fn estimate_wait_secs(pos: usize, queue: &[Job], estimator: &QueueEstimator) -> Option<u64> {
    let mut total = 0u64;
    let mut saw_any = false;
    for job in queue
        .iter()
        .take(pos)
        .filter(|job| render_queue_estimate_for_job(job) || active_encode_job(job))
    {
        let frames = estimator.frames_for_job(job).unwrap_or(None);
        let secs = if active_encode_job(job) {
            remaining_secs_active(job.encode_frame, job.encode_total, job.encode_fps)
                .or_else(|| remaining_secs_queued(frames, &job.preset))
        } else {
            remaining_secs_queued(frames, &job.preset)
        };
        total = total.saturating_add(secs.unwrap_or(UNKNOWN_JOB_ESTIMATE_SECS));
        saw_any = true;
    }
    if saw_any {
        Some(total)
    } else {
        Some(0)
    }
}

pub(crate) fn remaining_secs_active(frame: Option<u64>, total: Option<u64>, fps: Option<f64>) -> Option<u64> {
    let frame = frame?;
    let total = total?;
    let fps = fps?;
    if fps <= 0.0 || total <= frame {
        return None;
    }
    Some(((total - frame) as f64 / fps).ceil() as u64)
}

pub(crate) fn remaining_secs_queued(frames: Option<u64>, preset: &Preset) -> Option<u64> {
    let fps = match preset {
        Preset::PseudoLossless(_) => 30.0,
        Preset::Dummy(_) => 150.0,
        Preset::Standard(_) | Preset::Gpu(_) => 60.0,
    };
    frames.map(|frames| (frames as f64 / fps).ceil() as u64)
}

pub(crate) fn format_eta(secs: u64) -> String {
    let mins = secs.saturating_add(59) / 60;
    if mins < 60 {
        return format!("{}m", mins);
    }
    format!("{}h {:02}m", mins / 60, mins % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::p2p::nyaaise::TorrentType;

    fn encode_job(id: u64, ready: Stage, dispatched: bool) -> Job {
        let mut job = Job::new_api(
            0,
            0,
            JobType::Encode,
            TorrentType::Link("https://example.invalid/input.torrent".to_string()),
            Vec::new(),
            "EN".to_string(),
            None,
        );
        job.job_id = id;
        job.ready = ready;
        job.encode_dispatched = dispatched;
        job
    }

    #[test]
    fn remaining_secs_active_handles_normal_and_invalid_values() {
        assert_eq!(remaining_secs_active(Some(100), Some(220), Some(24.0)), Some(5));
        assert_eq!(remaining_secs_active(Some(220), Some(100), Some(24.0)), None);
        assert_eq!(remaining_secs_active(Some(100), Some(220), Some(0.0)), None);
    }

    #[test]
    fn remaining_secs_queued_uses_preset_fps() {
        assert_eq!(remaining_secs_queued(Some(300), &Preset::PseudoLossless(None)), Some(10));
        assert_eq!(remaining_secs_queued(Some(300), &Preset::Dummy(None)), Some(2));
        assert_eq!(remaining_secs_queued(Some(300), &Preset::Standard(None)), Some(5));
        assert_eq!(remaining_secs_queued(Some(300), &Preset::Gpu(None)), Some(5));
        assert_eq!(remaining_secs_queued(None, &Preset::Standard(None)), None);
    }

    #[test]
    fn format_eta_rounds_to_minutes() {
        assert_eq!(format_eta(0), "0m");
        assert_eq!(format_eta(1), "1m");
        assert_eq!(format_eta(65 * 60), "1h 05m");
    }

    #[test]
    fn downloaded_dispatched_job_still_gets_queue_eta() {
        let mut active = encode_job(1, Stage::Encoding, true);
        active.encode_frame = Some(100);
        active.encode_total = Some(220);
        active.encode_fps = Some(24.0);
        let waiting = encode_job(2, Stage::Downloaded, true);
        let queue = vec![active, waiting];

        assert!(render_queue_estimate_for_job(&queue[1]));
        assert_eq!(queue_position(1, &queue), 2);
        assert_eq!(estimate_wait_secs(1, &queue, &QueueEstimator::new()), Some(5));
    }
}
