use crate::lib::db::core::JobDb;
use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::API_PUBLIC_URL;
use crate::pnworker::core::{Job, JobType, Stage};
use crate::pnworker::frontend::Frontend;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs::{create_dir_all, write};

// One episode of a batch: the torrent file that carries it and the subtitle it was paired with in
// the confirmation embed. The bytes are kept so the child job can be built without going back to
// the staging directory the pairing was confirmed from.
#[derive(Clone, Debug)]
pub struct BatchEntry {
    pub file_index: u64,
    pub file_label: String,
    pub subtitle_name: String,
    pub subtitle: Vec<u8>,
    pub job_id: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct BatchRequest {
    pub entries: Vec<BatchEntry>,
    pub token: String,
    pub probe_job_id: u64,
    pub finished: usize,
    pub failed: usize,
    pub current: String,
    // Set once the download is over. Files the torrent never delivered can only be recognised at
    // that point, and without it the parent would wait forever for an episode that never arrives.
    pub download_settled: bool,
}

impl BatchRequest {
    pub fn new(entries: Vec<BatchEntry>, probe_job_id: u64) -> Self {
        Self {
            entries,
            token: batch_token(),
            probe_job_id,
            finished: 0,
            failed: 0,
            current: String::new(),
            download_settled: false,
        }
    }

    // Every entry that never became a child job is counted as failed exactly once, so a torrent
    // that delivered ten of twelve selected files still lets the batch reach its end.
    pub fn settle_download(&mut self) -> bool {
        if self.download_settled {
            return false;
        }
        self.download_settled = true;
        self.failed += self
            .entries
            .iter()
            .filter(|entry| entry.job_id.is_none())
            .count();
        true
    }

    pub fn total(&self) -> usize {
        self.entries.len()
    }

    pub fn file_indices(&self) -> Vec<u64> {
        self.entries.iter().map(|entry| entry.file_index).collect()
    }

    pub fn entry_for(&self, file_index: u64) -> Option<&BatchEntry> {
        self.entries
            .iter()
            .find(|entry| entry.file_index == file_index)
    }

    pub fn label_for_job(&self, job_id: u64) -> String {
        self.entries
            .iter()
            .find(|entry| entry.job_id == Some(job_id))
            .map(|entry| entry.file_label.clone())
            .unwrap_or_default()
    }

    pub fn complete(&self) -> bool {
        self.finished + self.failed >= self.total()
    }
}

fn batch_token() -> String {
    // A failed CSPRNG must not hand out a guessable capability, so the batch loses its page
    // instead: an empty token renders no output link.
    crate::lib::secret::random_hex_token().unwrap_or_default()
}

// Two children can be created inside the same nanosecond when several files finish together, so
// the clock alone is not a job id.
fn next_child_id() -> u64 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos() as u64)
        .unwrap_or(0);
    now.saturating_add(SEQUENCE.fetch_add(1, Ordering::Relaxed))
}

pub fn batch_token_dir() -> PathBuf {
    PathBuf::from("DB").join("config").join("global").join("batch")
}

// The output page is reached by capability, so the token has to outlive the process that minted
// it: the mapping is a file, not a memory registry.
pub async fn store_batch_token(token: &str, job_id: u64) {
    if token.is_empty() {
        return;
    }
    let dir = batch_token_dir();
    if create_dir_all(&dir).await.is_err() {
        return;
    }
    write(dir.join(token), job_id.to_string()).await.ok();
}

pub async fn batch_job_for_token(token: &str) -> Option<u64> {
    if !token.chars().all(|c| c.is_ascii_hexdigit()) || token.len() != 64 {
        return None;
    }
    tokio::fs::read_to_string(batch_token_dir().join(token))
        .await
        .ok()?
        .trim()
        .parse()
        .ok()
}

// The public origin is what makes the batch a one-message job: without it there is no page to link
// and every episode gets its own Discord message instead.
pub fn batch_output_url(token: &str) -> Option<String> {
    if token.is_empty() {
        return None;
    }
    let base = get_pandora_env().get(API_PUBLIC_URL)?.trim().to_string();
    if base.is_empty() {
        return None;
    }
    Some(format!("{}/batch/{}", base.trim_end_matches('/'), token))
}

pub fn batch_page_available() -> bool {
    get_pandora_env()
        .get(API_PUBLIC_URL)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

// The parent's stored progress is what the output page reads: the confirmed pairing plus the child
// job id each episode became, so the page can join it against the jobs table.
pub fn batch_progress_json(job: &Job) -> Option<String> {
    let batch = job.batch.as_ref()?;
    let entries: Vec<serde_json::Value> = batch
        .entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "index": entry.file_index,
                "label": entry.file_label,
                "subtitle": entry.subtitle_name,
                "job_id": entry.job_id.map(|id| id.to_string()),
            })
        })
        .collect();
    Some(
        serde_json::json!({
            "type": "batch",
            "token": batch.token,
            "total": batch.total(),
            "finished": batch.finished,
            "failed": batch.failed,
            "current": batch.current,
            "entries": entries,
        })
        .to_string(),
    )
}

pub async fn persist_batch_progress(db: &JobDb, job: &Job) {
    if let Some(progress) = batch_progress_json(job) {
        db.update_progress(job.job_id, &progress).await.ok();
    }
}

// Builds the encode job for one finished file. The download stays in the parent's torrent
// directory and is hard-linked into the child, so an episode that is already on disk is never
// copied twice and the parent can keep seeding the rest of its selection.
// Everything a batch child is, before anything touches the disk: the parent's settings, its own
// episode, and the file index that tells whoever runs it which one that is. `child.torrent` comes
// across with the clone, which is what makes a child describable to a node at all.
fn base_batch_child(parent: &Job, entry: &BatchEntry) -> Job {
    let mut child = parent.clone();
    child.job_id = next_child_id();
    child.job_type = JobType::Pancode;
    child.batch = None;
    child.batch_parent = Some(parent.job_id);
    child.probe_job_id = None;
    child.probe_file_index = Some(entry.file_index);
    child.probe_files = None;
    child.attachment = entry.subtitle.clone();
    child.acix = None;
    child.keep = None;
    child.keycode = None;
    child.preview = None;
    child.studio = None;
    child.duplicate_source = None;
    child.forward_parent = None;
    child.encode_warnings = Vec::new();
    child.encode_dispatched = false;
    child.encode_dispatch_order = None;
    child.encode_frame = None;
    child.encode_total = None;
    child.encode_fps = None;
    child.response_id = 0;
    child.frontend = Frontend::None;
    child.worker = "enc-pending".to_string();
    child.display_link = Some(format!(
        "{} • {}",
        parent
            .display_link
            .clone()
            .unwrap_or_else(|| parent.torrent.get()),
        entry.file_label
    ));
    child.directory = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("DB")
        .join("work")
        .join(child.job_id.to_string());
    child
}

// A child built to be handed to a node rather than encoded here: same episode, same settings, but
// no local file behind it. Its input is one index of the parent's torrent, which is the shape a
// leased Pancode already travels in, so it enters the queue at `Queued` and the node downloads that
// one file itself.
//
// This does no I/O at all. The work directory and the subtitle are `prepare_queued_job`'s to make,
// and it only runs once a node has actually been found — a child built for a cluster with nothing
// free would otherwise leave a directory behind on every pass of the loop.
pub fn leased_batch_child(parent: &Job, entry: &BatchEntry) -> Job {
    let mut child = base_batch_child(parent, entry);
    child.ready = Stage::Queued;
    child.worker = "link-pending".to_string();
    // A local child carries no probe id, so that the first one to finish cannot archive the probe
    // out from under its siblings. A leased one has to: if the node loses it, it comes back through
    // `queue_pancode_job`, which refuses a Pancode that names no probe, and the probe's saved
    // `.torrent` is how it would then fetch its own episode. Ending the shared probe is instead
    // refused where it would happen — a batch child never archives one, leased or not.
    child.probe_job_id = parent.batch.as_ref().map(|batch| batch.probe_job_id);
    child
}

pub async fn build_batch_child(parent: &Job, entry: &BatchEntry, source: &Path) -> Option<Job> {
    let mut child = base_batch_child(parent, entry);
    child.ready = Stage::Downloaded;

    for directory in crate::pnworker::core::STRUCT {
        if create_dir_all(child.directory.join(directory)).await.is_err() {
            return None;
        }
    }
    let torrent_dir = child.directory.join("contents").join("torrent");
    if create_dir_all(&torrent_dir).await.is_err() {
        return None;
    }
    let target = torrent_dir.join("input.mkv");
    if tokio::fs::hard_link(source, &target).await.is_err()
        && tokio::fs::copy(source, &target).await.is_err()
    {
        eprintln!(
            "[Pandora Batch] file {} could not be handed to a child job",
            source.display()
        );
        return None;
    }
    if write(
        child.directory.join("contents").join("subtitle.ass"),
        &child.attachment,
    )
    .await
    .is_err()
    {
        return None;
    }
    if let Some(watermark) = child.server_watermark.clone() {
        if write(
            child
                .directory
                .join("contents")
                .join("server_watermark.ass"),
            &watermark,
        )
        .await
        .is_err()
        {
            return None;
        }
    }
    if let Some(logo) = child.server_logo.clone() {
        if crate::pnworker::server_effects::write_job_logo(&child.directory, &logo)
            .await
            .is_err()
        {
            return None;
        }
    }
    Some(child)
}

// The deprioritization contract: a batch child never runs while another batch child is encoding,
// and it yields to ordinary encodes until two of them have gone ahead of it. Without the second
// half the batch would starve behind a busy queue; without the first it would take the encoder for
// its whole selection.
pub fn batch_child_may_dispatch(
    batch_in_flight: bool,
    others_waiting: bool,
    others_since_batch: u64,
) -> bool {
    if batch_in_flight {
        return false;
    }
    !others_waiting || others_since_batch >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(index: u64, label: &str) -> BatchEntry {
        BatchEntry {
            file_index: index,
            file_label: label.to_string(),
            subtitle_name: format!("{index}.ass"),
            subtitle: Vec::new(),
            job_id: None,
        }
    }

    #[test]
    fn batch_holds_one_encoder_slot_at_a_time() {
        assert!(!batch_child_may_dispatch(true, false, 9));
        assert!(!batch_child_may_dispatch(true, true, 9));
    }

    #[test]
    fn batch_yields_to_two_ordinary_encodes_then_takes_its_turn() {
        assert!(!batch_child_may_dispatch(false, true, 0));
        assert!(!batch_child_may_dispatch(false, true, 1));
        assert!(batch_child_may_dispatch(false, true, 2));
    }

    #[test]
    fn batch_runs_back_to_back_when_nothing_else_is_waiting() {
        assert!(batch_child_may_dispatch(false, false, 0));
    }

    #[test]
    fn tokens_are_unguessable_and_round_trip_as_hex() {
        let token = batch_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(token, batch_token());
    }

    #[test]
    fn a_file_the_torrent_never_delivered_still_ends_the_batch() {
        let mut request = BatchRequest::new(vec![entry(3, "E01"), entry(4, "E02")], 7);
        request.entries[0].job_id = Some(42);
        request.finished = 1;
        assert!(!request.complete());
        assert!(request.settle_download());
        assert_eq!(request.failed, 1);
        assert!(request.complete());
        // Settling twice must not double-count the same missing episode.
        assert!(!request.settle_download());
        assert_eq!(request.failed, 1);
    }

    #[test]
    fn progress_carries_the_pairing_and_child_ids() {
        let mut request = BatchRequest::new(vec![entry(3, "E01"), entry(4, "E02")], 7);
        request.entries[0].job_id = Some(42);
        request.finished = 1;
        assert_eq!(request.total(), 2);
        assert_eq!(request.file_indices(), vec![3, 4]);
        assert_eq!(request.label_for_job(42), "E01");
        assert!(!request.complete());
        request.failed = 1;
        assert!(request.complete());
    }

    // A leased child is the same episode as a local one, described so that a machine which has
    // never seen this torrent can fetch exactly that file and nothing else.
    #[test]
    fn a_leased_child_carries_its_parents_source_and_its_own_index() {
        let mut parent = Job::new_api(
            0,
            0,
            JobType::Batch,
            crate::lib::p2p::nyaaise::TorrentType::Magnet("magnet:?xt=urn:btih:abc".to_string()),
            Vec::new(),
            "en".to_string(),
            None,
        );
        let mut only = entry(7, "Episode 07");
        only.subtitle = b"subs".to_vec();
        parent.batch = Some(BatchRequest::new(vec![only.clone()], 4242));

        let child = leased_batch_child(&parent, &only);

        assert_eq!(child.torrent.get(), parent.torrent.get(), "the source travels");
        assert_eq!(child.probe_file_index, Some(7));
        assert_eq!(child.batch_parent, Some(parent.job_id));
        assert!(child.batch.is_none(), "a child is not itself a batch");
        // Queued, not Downloaded: there is no local file behind it, which is the whole difference
        // between this and the child the parent's own download produces.
        assert_eq!(child.ready, Stage::Queued);
        // Carried so a node's `queue_pancode_job` accepts it, and so a lost lease has somewhere to
        // come back to. Nothing archives it: a batch child never ends its parent's probe.
        assert_eq!(child.probe_job_id, Some(4242));
        assert_eq!(child.attachment, b"subs".to_vec());
    }
}
