use crate::pnworker::core::{Job, JobType};
use crate::pnworker::link::board;
use crate::pnworker::link::client::{encode_base64, job_type_is_leasable};
use crate::pnworker::link::spec::{
    LinkJobSpec, job_type_name, preset_name, source_to_wire,
};

// The coordinator's side of deciding what leaves this machine. Everything here is a pure question
// about one job; the loop in `core.rs` owns the consequences, because only it may touch the queue.

// A job that reliably kills whatever runs it must not become an endless tour of the cluster. Past
// this many losses it stays local, where the encode stall watchdog can end it properly.
pub const LINK_MAX_ATTEMPTS: u32 = 2;

// Which jobs can run on a node at all. The rule is simply "does it carry its own source": a node
// fetches its own input, so anything whose input is already on this machine cannot be leased.
pub fn is_eligible(job: &Job) -> bool {
    if !job_type_is_leasable(job.job_type) {
        return false;
    }
    if job.link_attempts >= LINK_MAX_ATTEMPTS {
        return false;
    }
    // A forwarded job runs nowhere — it mirrors another job's outcome.
    if job.forward_parent.is_some() {
        return false;
    }
    // A batch parent owns one torrent download feeding many children, and a child is born already
    // downloaded out of it. Neither carries a source a node could fetch on its own.
    if job.batch.is_some() || job.batch_parent.is_some() {
        return false;
    }
    // A keep leaves its output on this machine for a later `/keycode` to join, and Keycode itself
    // reads those local files. Preview and Studio are the same shape.
    if job.keep.is_some() || job.keycode.is_some() || job.preview.is_some() || job.studio.is_some()
    {
        return false;
    }
    // A node fetches its own source, so there has to be one. A Pancode whose only route to its
    // input is the `.torrent` its probe saved on this machine cannot be leased — the node has no
    // probe job and nothing to adopt.
    if job.torrent.get().trim().is_empty() {
        return false;
    }
    // A GDrive or direct-HTTP source is a URL like any other and travels fine; a probe-derived
    // Pancode carries its torrent as a link too, and only the file index alongside it.
    true
}

// The node this job should go to, or None to keep it local. A pinned job is only ever offered to
// the node it names — and waits for it, which is the whole point of pinning.
pub fn choose_node(job: &Job) -> Option<String> {
    if !is_eligible(job) {
        return None;
    }
    board::pick_node(&preset_name(&job.preset), job.link_pin.as_deref())
}

// Everything the node needs, resolved. Nothing here is an id for the node to look up: the server's
// preset, watermark and Drive folders were snapshotted onto the job when it was created, and this
// is that same snapshot travelling one hop further.
pub fn build_spec(job: &Job, expires_at: u64, renew_secs: u64, return_output: bool) -> LinkJobSpec {
    let (source_kind, source) = source_to_wire(&job.torrent);
    LinkJobSpec {
        job_id: job.job_id.to_string(),
        job_type: job_type_name(job.job_type),
        source_kind,
        source,
        display_link: job.display_link.clone(),
        file_index: job.probe_file_index,
        probe_job_id: job.probe_job_id.map(|id| id.to_string()),
        subtitle_b64: encode_base64(&job.attachment),
        watermark_b64: job.server_watermark.as_deref().map(encode_base64),
        preset: preset_name(&job.preset),
        lang: job.lang.clone(),
        server_id: job.server_id.map(|id| id.to_string()),
        gdrive_folder_global: job.gdrive_folder_global.clone(),
        gdrive_folder_local: job.gdrive_folder_local.clone(),
        return_output,
        expires_at,
        renew_secs,
    }
}

// The worker label a leased job wears. `/workers` and the job embed both render `Job.worker`
// verbatim, so this is where an operator finds out which machine has their episode.
pub fn worker_label(node: &str) -> String {
    format!("lnk-{node}")
}

pub fn is_link_worker(worker: &str) -> bool {
    worker.starts_with("lnk-")
}

pub fn leasable_job_types() -> [JobType; 4] {
    [JobType::Encode, JobType::Pancode, JobType::Backup, JobType::Probe]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::p2p::nyaaise::TorrentType;
    use crate::pnworker::core::{Job, KeepRequest};

    fn job(job_type: JobType) -> Job {
        Job::new_api(
            0,
            0,
            job_type,
            TorrentType::Magnet("magnet:?xt=urn:btih:abc".to_string()),
            Vec::new(),
            "en".to_string(),
            None,
        )
    }

    #[test]
    fn an_ordinary_encode_with_a_source_is_eligible() {
        assert!(is_eligible(&job(JobType::Encode)));
    }

    // A node fetches its own input. A job whose only route to its source is a file on this machine
    // has nothing to give it.
    #[test]
    fn a_job_with_no_source_link_is_not_eligible() {
        let mut job = job(JobType::Pancode);
        job.torrent = TorrentType::Link("   ".to_string());
        assert!(!is_eligible(&job));
    }

    // A batch child is hard-linked out of its parent's single torrent download, so it carries no
    // source at all; a parent is a download feeding children and encodes nothing itself.
    #[test]
    fn batch_parents_and_children_stay_local() {
        let mut child = job(JobType::Encode);
        child.batch_parent = Some(42);
        assert!(!is_eligible(&child));
    }

    // A keep leaves its output here for a later `/keycode` to join.
    #[test]
    fn a_keep_job_stays_local() {
        let mut job = job(JobType::Encode);
        job.keep = Some(KeepRequest::new(Some("kw".to_string())));
        assert!(!is_eligible(&job));
    }

    // A forwarded job runs nowhere — it mirrors another job's outcome.
    #[test]
    fn a_forwarded_job_is_never_leased() {
        let mut job = job(JobType::Encode);
        job.forward_parent = Some(7);
        assert!(!is_eligible(&job));
    }

    // A job that kills whatever runs it must stop touring the cluster and end where the stall
    // watchdog can reach it.
    #[test]
    fn a_job_out_of_attempts_stays_local() {
        let mut job = job(JobType::Encode);
        job.link_attempts = LINK_MAX_ATTEMPTS;
        assert!(!is_eligible(&job));
    }

    // The spec is the node's only source of truth, so what the coordinator snapshotted has to be
    // in it — including the probe reference a Pancode needs to survive `queue_pancode_job`.
    #[test]
    fn the_spec_carries_the_whole_snapshot() {
        let mut source = job(JobType::Pancode);
        source.attachment = b"[Script Info]".to_vec();
        source.server_watermark = Some(b"[V4+ Styles]".to_vec());
        source.probe_file_index = Some(7);
        source.probe_job_id = Some(555);
        source.gdrive_folder_local = Some("folder".to_string());

        let spec = build_spec(&source, 100, 10, false);
        assert_eq!(spec.job_id, source.job_id.to_string());
        assert_eq!(spec.job_type, "Pancode");
        assert_eq!(spec.source_kind, "magnet");
        assert_eq!(spec.file_index, Some(7));
        assert_eq!(spec.probe_job_id.as_deref(), Some("555"));
        assert_eq!(spec.gdrive_folder_local.as_deref(), Some("folder"));
        assert!(!spec.subtitle_b64.is_empty());
        assert!(spec.watermark_b64.is_some());
        assert_eq!(spec.expires_at, 100);
    }

    #[test]
    fn every_leasable_job_type_passes_the_client_gate() {
        for job_type in leasable_job_types() {
            assert!(job_type_is_leasable(job_type));
        }
    }

    #[test]
    fn the_worker_label_carries_the_node_name() {
        let label = worker_label("mini-osaka");
        assert!(is_link_worker(&label));
        assert!(label.contains("mini-osaka"));
    }
}
