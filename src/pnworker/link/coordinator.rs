use crate::pnworker::core::{Job, JobType, Preset};
use crate::pnworker::util::IntrosConfig;
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
//
// `has_source_file` is the caller's answer to "is there a `.torrent` on this machine we can send
// with it". It is a parameter rather than a check here because this module is deliberately free of
// I/O — every other question it answers is about the job struct alone.
pub fn is_eligible(job: &Job, has_source_file: bool) -> bool {
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
    // A batch parent owns one torrent download feeding many children; leasing it would put the
    // download on a node and the children that hard-link out of it on this machine.
    //
    // A *child* is a different matter. It is a Pancode carrying its parent's torrent and one file
    // index, which is exactly the shape a leased Pancode already travels in, so a node can fetch
    // that one episode itself. What it cannot do is be leased after the parent has already
    // downloaded it — the coordinator offers children before that, and `spawn_batch_child` skips an
    // entry that is already someone's.
    if job.batch.is_some() {
        return false;
    }
    // A keep leaves its output on this machine for a later `/keycode` to join, and Keycode itself
    // reads those local files. Preview and Studio are the same shape.
    if job.keep.is_some() || job.keycode.is_some() || job.preview.is_some() || job.studio.is_some()
    {
        return false;
    }
    // A node fetches its own source, so there has to be one — either a link it can resolve or a
    // `.torrent` this machine can hand over with the job. A Pancode whose only route to its input
    // was the file its probe saved here used to be refused for exactly this reason.
    if job.torrent.get().trim().is_empty() && !has_source_file {
        return false;
    }
    // A GDrive or direct-HTTP source is a URL like any other and travels fine; a probe-derived
    // Pancode carries its torrent as a link too, and only the file index alongside it.
    true
}

// The node this job should go to, or None to keep it local. A pinned job is only ever offered to
// the node it names — and waits for it, which is the whole point of pinning.
pub fn choose_node(job: &Job, has_source_file: bool) -> Option<String> {
    if !is_eligible(job, has_source_file) {
        return None;
    }
    board::pick_node(
        &preset_name(&job.preset),
        job.link_pin.as_deref(),
        job.server_id,
    )
}

// Everything the node needs, resolved. Nothing here is an id for the node to look up: the server's
// preset, watermark and Drive folders were snapshotted onto the job when it was created, and this
// is that same snapshot travelling one hop further.
// What the node needs to find this job's input, when the job struct alone does not say it.
//
// A batch child is the case that needs both halves: its input is one index of its parent's torrent,
// the probe that produced the file list belongs to the parent, and the metainfo may be a file on
// this machine that no link reaches. Neither can be read off the child, because the child
// deliberately holds no probe id of its own — the first one to finish would archive the probe out
// from under its siblings.
#[derive(Clone, Debug, Default)]
pub struct LeaseSource {
    pub torrent_file: Option<Vec<u8>>,
    pub probe_job_id: Option<u64>,
}

impl LeaseSource {
    pub fn has_file(&self) -> bool {
        self.torrent_file.as_ref().is_some_and(|bytes| !bytes.is_empty())
    }
}

pub fn build_spec(
    job: &Job,
    source_extras: &LeaseSource,
    expires_at: u64,
    renew_secs: u64,
    return_output: bool,
    drive_only: bool,
) -> LinkJobSpec {
    let (source_kind, source) = source_to_wire(&job.torrent);
    // The concat folder lives inside the preset variant, and `preset_name` cannot carry it: it is a
    // path on this machine and means nothing on another. What travels is the group's name, which
    // the node resolves against the copy it synced.
    let intro_group = intro_candidates(&job.preset)
        .and_then(|folder| IntrosConfig::load().group_for_folder(&folder));
    LinkJobSpec {
        job_id: job.job_id.to_string(),
        job_type: job_type_name(job.job_type),
        source_kind,
        source,
        display_link: job.display_link.clone(),
        file_index: job.probe_file_index,
        // The child's own probe id is deliberately absent, so the parent's travels instead: without
        // one the node's `queue_pancode_job` refuses the job outright, and adopting it over there
        // simply fails and falls back to the source beside it.
        probe_job_id: source_extras
            .probe_job_id
            .or(job.probe_job_id)
            .map(|id| id.to_string()),
        torrent_b64: source_extras
            .torrent_file
            .as_deref()
            .filter(|bytes| !bytes.is_empty())
            .map(encode_base64),
        subtitle_b64: encode_base64(&job.attachment),
        watermark_b64: job.server_watermark.as_deref().map(encode_base64),
        preset: preset_name(&job.preset),
        lang: job.lang.clone(),
        server_id: job.server_id.map(|id| id.to_string()),
        gdrive_folder_global: job.gdrive_folder_global.clone(),
        gdrive_folder_local: job.gdrive_folder_local.clone(),
        return_output,
        drive_only,
        intro_group,
        assets_revision: crate::pnworker::link::assets::manifest().revision,
        expires_at,
        renew_secs,
    }
}

// The intro/concat folder a preset carries. Every encoding variant holds one; `Copy` never does.
pub fn intro_candidates(preset: &Preset) -> Option<String> {
    match preset {
        preset => preset.candidates().cloned(),
    }
    .filter(|folder| !folder.trim().is_empty())
}

// The worker label a leased job wears. `/workers` and the job embed both render `Job.worker`
// verbatim, so this is where an operator finds out which machine has their episode — or, when the
// machine belongs to a `/teenode` group, which group. Only the label merges: the roster, the lease,
// and every scheduling decision still address the node by its own name, and the snapshot's node
// view carries both, so a stall is still traceable to one box.
pub fn worker_label(node: &str) -> String {
    format!("lnk-{}", crate::pnworker::link::board::display_name(node))
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
        assert!(is_eligible(&job(JobType::Encode), false));
    }

    // A node fetches its own input. A job with neither a source link nor a `.torrent` this machine
    // can hand over has nothing to give it — and with the file, the same job travels fine.
    #[test]
    fn a_job_with_no_source_link_needs_a_torrent_file_to_travel() {
        let mut job = job(JobType::Pancode);
        job.torrent = TorrentType::Link("   ".to_string());
        assert!(!is_eligible(&job, false));
        assert!(is_eligible(&job, true));
    }

    // A parent is one download feeding children that hard-link out of it: leasing it would put the
    // download on a node and the encodes here. A child carries its parent's torrent and one file
    // index, which is a source a node can fetch on its own.
    #[test]
    fn batch_parents_stay_local_but_children_travel() {
        let mut parent = job(JobType::Batch);
        parent.batch = Some(crate::pnworker::batch::BatchRequest::new(Vec::new(), 1));
        assert!(!is_eligible(&parent, true));

        let mut child = job(JobType::Pancode);
        child.batch_parent = Some(42);
        child.probe_file_index = Some(3);
        assert!(is_eligible(&child, false), "a child with a magnet needs no file");
    }

    // A keep leaves its output here for a later `/keycode` to join.
    #[test]
    fn a_keep_job_stays_local() {
        let mut job = job(JobType::Encode);
        job.keep = Some(KeepRequest::new(Some("kw".to_string())));
        assert!(!is_eligible(&job, false));
    }

    // A forwarded job runs nowhere — it mirrors another job's outcome.
    #[test]
    fn a_forwarded_job_is_never_leased() {
        let mut job = job(JobType::Encode);
        job.forward_parent = Some(7);
        assert!(!is_eligible(&job, false));
    }

    // A job that kills whatever runs it must stop touring the cluster and end where the stall
    // watchdog can reach it.
    #[test]
    fn a_job_out_of_attempts_stays_local() {
        let mut job = job(JobType::Encode);
        job.link_attempts = LINK_MAX_ATTEMPTS;
        assert!(!is_eligible(&job, false));
    }

    // The spec is the node's only source of truth, so what the coordinator snapshotted has to be
    // in it — including the probe reference a Pancode needs to survive `queue_pancode_job`.
    // The concat folder lives inside the preset variant and is easy to drop on the way out — which
    // is exactly what happened before this existed, leaving leased jobs with no intro at all.
    #[test]
    fn every_encoding_preset_yields_its_concat_folder() {
        let folder = Some("DB/intros/summer".to_string());
        for preset in [
            Preset::Standard(folder.clone()),
            Preset::VerySlow(folder.clone()),
            Preset::Gpu(folder.clone()),
            Preset::Av1(folder.clone()),
            Preset::PseudoLossless(folder.clone()),
            Preset::Dummy(folder.clone()),
            Preset::Hd720(folder.clone()),
            Preset::Sd480(folder.clone()),
        ] {
            assert_eq!(intro_candidates(&preset).as_deref(), Some("DB/intros/summer"));
        }
        assert_eq!(intro_candidates(&Preset::Copy), None);
        assert_eq!(intro_candidates(&Preset::Standard(None)), None);
        // A blank folder is no folder; it must not travel as a group name to look up.
        assert_eq!(intro_candidates(&Preset::Standard(Some("  ".to_string()))), None);
    }

    // A node refuses a job whose corpus it cannot prove it holds, so the spec has to name one.
    #[test]
    fn the_spec_names_the_asset_revision_it_was_built_against() {
        let spec = build_spec(&job(JobType::Encode), &LeaseSource::default(), 100, 10, false, false);
        assert!(!spec.assets_revision.is_empty());
    }

    // A node holds no `meta.pandora` for the originating guild, so both upload policies have to
    // travel with the job. Dropping either publishes to hosts the server switched off, or puts a
    // playback URL on a machine with no public hostname.
    #[test]
    fn the_spec_carries_the_servers_upload_policy() {
        let source = job(JobType::Encode);
        let plain = build_spec(&source, &LeaseSource::default(), 100, 10, false, false);
        assert!(!plain.return_output);
        assert!(!plain.drive_only);

        let restricted = build_spec(&source, &LeaseSource::default(), 100, 10, true, true);
        assert!(restricted.return_output);
        assert!(restricted.drive_only);
    }

    #[test]
    fn the_spec_carries_the_whole_snapshot() {
        let mut source = job(JobType::Pancode);
        source.attachment = b"[Script Info]".to_vec();
        source.server_watermark = Some(b"[V4+ Styles]".to_vec());
        source.probe_file_index = Some(7);
        source.probe_job_id = Some(555);
        source.gdrive_folder_local = Some("folder".to_string());

        let spec = build_spec(&source, &LeaseSource::default(), 100, 10, false, false);
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

    // A batch child holds no probe id of its own and its metainfo may be a file nobody else can
    // reach. Both travel in the spec instead, or the node has no way to find the episode at all.
    #[test]
    fn a_lease_carries_the_torrent_and_probe_its_job_cannot_name() {
        let mut child = job(JobType::Pancode);
        child.torrent = TorrentType::Link(String::new());
        child.batch_parent = Some(42);
        child.probe_file_index = Some(3);
        child.probe_job_id = None;

        let extras = LeaseSource {
            torrent_file: Some(b"d8:announce".to_vec()),
            probe_job_id: Some(99),
        };
        assert!(extras.has_file());
        let spec = build_spec(&child, &extras, 100, 10, false, false);

        assert_eq!(spec.probe_job_id.as_deref(), Some("99"));
        assert_eq!(spec.file_index, Some(3));
        assert_eq!(
            spec.torrent_b64.as_deref(),
            Some(encode_base64(b"d8:announce")).as_deref()
        );

        // With nothing to send, the field stays absent rather than travelling as an empty string a
        // node would have to decide the meaning of.
        let plain = build_spec(&child, &LeaseSource::default(), 100, 10, false, false);
        assert!(plain.torrent_b64.is_none());
        assert!(!LeaseSource::default().has_file());
    }
}
