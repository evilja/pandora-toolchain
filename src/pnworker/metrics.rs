//! Monthly aggregate metrics. Job measurements exist only in memory and lease reports.
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use serde::{Deserialize, Serialize};
use crate::lib::sync::lock;
use crate::pnworker::core::{Job, JobType};
use crate::pnworker::messages::{MessagePayload, ENCODE_START, ENCODE_PROG, ENCODE_DONE};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Totals {
    pub total_frames: u64,
    pub linear_aot_frames: u64,
    pub cache_saved_bytes: u64,
    pub encode_millis: u64,
    pub linear_aot_encode_millis: u64,
    pub uploaded_bytes: u64,
    pub successful_encodes: u64,
}

impl Totals {
    fn values(self) -> [u64; 7] {
        [self.total_frames, self.linear_aot_frames, self.cache_saved_bytes, self.encode_millis,
         self.linear_aot_encode_millis, self.uploaded_bytes, self.successful_encodes]
    }
    fn from_values(v: [u64; 7]) -> Self {
        Self { total_frames: v[0], linear_aot_frames: v[1], cache_saved_bytes: v[2],
            encode_millis: v[3], linear_aot_encode_millis: v[4], uploaded_bytes: v[5], successful_encodes: v[6] }
    }
    fn merge(self, other: Self) -> Self {
        let a = self.values(); let b = other.values();
        Self::from_values(std::array::from_fn(|i| a[i].max(b[i])))
    }
    fn delta(self, previous: Self) -> Self {
        let a = self.values(); let b = previous.values();
        Self::from_values(std::array::from_fn(|i| a[i].saturating_sub(b[i])))
    }
    fn add(self, other: Self) -> Self {
        let a = self.values(); let b = other.values();
        Self::from_values(std::array::from_fn(|i| a[i].saturating_add(b[i])))
    }
}

#[derive(Default)]
struct Measurement { totals: Totals, started: Option<u64>, frames: u64 }
fn measurements() -> &'static Mutex<HashMap<u64, Measurement>> {
    static STATE: OnceLock<Mutex<HashMap<u64, Measurement>>> = OnceLock::new();
    STATE.get_or_init(Default::default)
}
fn now_millis() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}
pub(crate) fn snapshot(id: u64) -> Totals {
    lock(measurements()).get(&id).map(|m| m.totals).unwrap_or_default()
}
pub(crate) fn receive(id: u64, totals: Totals) {
    let mut state = lock(measurements());
    let m = state.entry(id).or_default();
    m.totals = m.totals.merge(totals);
}
pub(crate) fn cache_hit(id: u64, bytes: u64) {
    let mut state = lock(measurements());
    let m = state.entry(id).or_default();
    m.totals.cache_saved_bytes = m.totals.cache_saved_bytes.max(bytes);
}
pub(crate) fn uploaded(id: u64, bytes: u64) {
    let mut state = lock(measurements());
    let m = state.entry(id).or_default();
    m.totals.uploaded_bytes = m.totals.uploaded_bytes.saturating_add(bytes);
}

pub(crate) async fn observe(job: &Job, payload: &MessagePayload) {
    if job.forward_parent.is_some() || !matches!(job.job_type, JobType::Encode | JobType::Pancode | JobType::Keycode | JobType::Studio) {
        return;
    }
    // Remote measurements come from the accepted lease report. Never probe this machine's empty
    // work directory or time network delivery as though it were encoding.
    if job.link_node.is_none() {
        let id = match payload { MessagePayload::Static(id) | MessagePayload::Progress(id, _) => *id };
        if id == ENCODE_START {
            lock(measurements()).entry(job.job_id).or_default().started = Some(now_millis());
        } else if id == ENCODE_PROG {
            if let MessagePayload::Progress(_, args) = payload {
                let frame = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                let mut state = lock(measurements());
                let m = state.entry(job.job_id).or_default();
                m.frames = m.frames.max(frame);
            }
        } else if id == ENCODE_DONE && snapshot(job.job_id).successful_encodes == 0 {
            let ended = now_millis();
            let work = job.directory.join("work");
            let frames = tokio::task::spawn_blocking(move || {
                let output = work.join("output.mp4");
                let output = if output.is_file() { Some(output) } else {
                    std::fs::read_dir(work.join("hls")).ok().and_then(|entries| {
                        entries.filter_map(Result::ok).map(|e| e.path())
                            .find(|p| p.extension().is_some_and(|ext| ext == "m3u8"))
                    })
                }?;
                crate::lib::mpeg::probe::ffprobe_frame(&output.display().to_string())
            }).await.ok().flatten();
            let aot = tokio::fs::read_to_string(job.directory.join("work").join("linear-aot.metrics"))
                .await.ok().and_then(|v| {
                    let nums = v.split_whitespace().map(str::parse::<u64>).collect::<Result<Vec<_>, _>>().ok()?;
                    (nums.len() == 3).then_some(nums)
                });
            let mut state = lock(measurements());
            let m = state.entry(job.job_id).or_default();
            m.totals.total_frames = frames.unwrap_or(m.frames);
            let mut started = m.started.unwrap_or(ended);
            if let Some(aot) = aot {
                m.totals.linear_aot_frames = aot[0];
                m.totals.linear_aot_encode_millis = aot[2];
                if aot[1] != 0 { started = started.min(aot[1]); }
                m.totals.total_frames = m.totals.total_frames.max(aot[0]);
            }
            m.totals.encode_millis = ended.saturating_sub(started);
            m.totals.successful_encodes = 1;
        }
    }
    if !crate::pnworker::link::client::is_mini() {
        if let Err(error) = persist(job.job_id, snapshot(job.job_id)).await {
            eprintln!("[metrics] monthly totals could not be saved: {error}");
        }
    }
}

// Gregorian calendar from Unix days; months are UTC, independent of node timezone.
fn month(unix_millis: u64) -> (i64, i64) {
    let z = (unix_millis / 86_400_000) as i64 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (yoe + era * 400 + i64::from(month <= 2), month)
}
fn applied() -> &'static tokio::sync::Mutex<HashMap<u64, Totals>> {
    static APPLIED: OnceLock<tokio::sync::Mutex<HashMap<u64, Totals>>> = OnceLock::new();
    APPLIED.get_or_init(Default::default)
}

pub(crate) async fn forget(id: u64) {
    lock(measurements()).remove(&id);
    applied().lock().await.remove(&id);
}

async fn persist(id: u64, totals: Totals) -> Result<(), String> {
    let mut applied = applied().lock().await;
    let previous = applied.get(&id).copied().unwrap_or_default();
    let delta = totals.delta(previous);
    if delta == Totals::default() { return Ok(()); }
    let (year, month) = month(now_millis());
    let directory = std::path::PathBuf::from("DB").join("metrics").join(format!("{year:04}")).join(format!("{month:02}"));
    tokio::task::spawn_blocking(move || update_file(&directory, delta)).await.map_err(|e| e.to_string())??;
    applied.insert(id, previous.merge(totals));
    Ok(())
}
fn update_file(directory: &Path, delta: Totals) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let path = directory.join("metrics.pandora");
    let previous = match std::fs::read(&path) {
        Ok(data) => serde_json::from_slice::<Totals>(&data).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Totals::default(),
        Err(e) => return Err(e.to_string()),
    };
    let temporary = directory.join(".metrics.pandora.tmp");
    use std::io::Write;
    let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec_pretty(&previous.add(delta)).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_and_out_of_order_reports_have_no_extra_delta() {
        let first = Totals { total_frames: 200, successful_encodes: 1, ..Default::default() };
        let next = Totals { uploaded_bytes: 700, ..first };
        assert_eq!(first.delta(first), Totals::default());
        assert_eq!(first.merge(next).delta(first).uploaded_bytes, 700);
        assert_eq!(next.merge(first).delta(next), Totals::default());
    }
    #[test]
    fn node_reports_keep_metrics_and_accept_older_protocol_reports() {
        use crate::pnworker::link::spec::LinkReport;
        let old: LinkReport = serde_json::from_str(r#"{"payload":{"id":"ENCODE_DONE","args":null},"stage":"encoded"}"#).unwrap();
        assert!(old.metrics.is_none());
        let totals = Totals { total_frames: 321, uploaded_bytes: 654, successful_encodes: 1, ..Default::default() };
        let report = LinkReport { metrics: Some(totals), ..old };
        let wire = serde_json::to_string(&report).unwrap();
        let received: LinkReport = serde_json::from_str(&wire).unwrap();
        assert_eq!(received.metrics, Some(totals));
        receive(u64::MAX, totals);
        receive(u64::MAX, totals);
        receive(u64::MAX, Totals::default());
        assert_eq!(snapshot(u64::MAX), totals);
        lock(measurements()).remove(&u64::MAX);
    }

    #[test]
    fn months_roll_at_utc_boundaries() {
        assert_eq!(month(0), (1970, 1));
        assert_eq!(month(1_706_745_600_000), (2024, 2));
        assert_eq!(month(1_709_251_200_000), (2024, 3));
    }
    #[test]
    fn totals_survive_restart_and_corruption_is_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("pandora-metrics-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let delta = Totals { total_frames: 12, successful_encodes: 1, ..Default::default() };
        update_file(&dir, delta).unwrap();
        update_file(&dir, delta).unwrap();
        let path = dir.join("metrics.pandora");
        let total: Totals = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(total.total_frames, 24);
        assert_eq!(total.successful_encodes, 2);
        std::fs::write(&path, b"broken").unwrap();
        assert!(update_file(&dir, delta).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"broken");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
