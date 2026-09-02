use serde::{Deserialize, Serialize};

use crate::lib::db::core::{stage_label, stage_to_int};
use crate::lib::mpeg::preset::PresetHardware;
use crate::lib::p2p::nyaaise::TorrentType;
use crate::pnworker::core::{JobType, Preset, Stage};
use crate::pnworker::messages::{MessagePayload, intern_message_id};

// The wire contract between a coordinator and a Pandora Mini node. Both sides compile this same
// file, so a field added to one is added to the other; nothing here is versioned by hand. Job ids
// and server ids travel as strings because they are Discord snowflakes and nanosecond timestamps,
// and a JSON number cannot carry either without loss.

// What a node reports about itself when it comes up. `encoder_identity` names the libx264 it
// encodes with — build number, point version, and whether it is the Pandora fork — because that,
// and nothing else about the binary, is what decides an encode. Two nodes with different Rust
// compilers, libcs or host distributions produce identical output as long as this matches.
//
// The coordinator refuses a mismatch by default: an episode is encoded entirely on one machine so
// a mismatch cannot corrupt a file, but two x264 builds make different rate decisions at the same
// CRF, and a cluster that quietly ships two quality tiers is not worth debugging later. It is
// `#[serde(default)]` so a node from before this field existed deserialises with an empty identity
// and is refused with a reason, rather than failing to parse at all.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeRegister {
    pub node: String,
    pub pandora_version: String,
    #[serde(default)]
    pub encoder_identity: String,
    pub ffmpeg_version: String,
    pub threads: u32,
    pub max_jobs: u32,
    // Hardware encoders this machine proved with a real test encode. `alias = "presets"` reads
    // the placeholder field older nodes sent, but an empty list grants no GPU capability.
    #[serde(default, alias = "presets")]
    pub encoders: Vec<String>,
    // The build this node last recorded itself level with. Reported rather than enforced: the
    // coordinator shows it on `/lsnode` so a node that is failing to update is visible as a number
    // that stopped moving, which nothing else in the roster would reveal.
    #[serde(default)]
    pub build: u64,
    // A migration this node could not run. It travels on register because that is the first call
    // after the restart the migration was part of, and it stays set until a later run clears it —
    // otherwise the one machine that failed to migrate is also the one machine nobody hears from
    // about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeRegistered {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub renew_secs: u64,
    pub lease_timeout_secs: u64,
    // The corpus the node should hold. It reconciles against this before it polls for work.
    #[serde(default)]
    pub assets_revision: String,
    // What this node is for, as decided by its token rather than by anything it said. Echoed back
    // so the node can log what the cluster believes about it — a machine with a GPU that is
    // registered as `cpu` is a token that needs re-minting, and it should be visible on the node
    // as well as on the coordinator.
    #[serde(default)]
    pub purpose: NodePurpose,
    // Where the coordinator is. Sent here too, and not only from `/link/release`, so a node that
    // has just come up is told before it polls for its first job rather than after.
    #[serde(default)]
    pub release: ReleaseInfo,
    // Whether this node is drained. It rides the renew answer as well, but a renew is a thing only
    // a working node sends: an idle one would never hear that it had been let back in, and the
    // flag on its side latched at whatever the last job it held was told.
    #[serde(default)]
    pub drain: bool,
}

// What the coordinator is running, and what a node compares itself against.
//
// `version` and `build` are the comparison; `commit` is the thing to move to. Version alone cannot
// serve — it changes when someone edits Cargo.toml, not when a deploy happens — and commit alone
// would work but says nothing about direction, so a report cannot tell "behind" from "diverged".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseInfo {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub build: u64,
    #[serde(default)]
    pub commit: String,
    // Set by `/gitforce`. A node resets onto `commit` rather than fast-forwarding towards it,
    // which is the only way back for a checkout that has diverged from the coordinator's.
    #[serde(default)]
    pub reset: bool,
}

// What a node is for. It comes from the node's own token — `<token>|link|<node>|gpu` — and never
// from anything the node reports, so a machine cannot promote itself into work it has no hardware
// for by editing its own config.
//
// A token that names no purpose means CPU. That is a real answer rather than a fallback: the
// machines that predate this field are the CPU boxes the cluster was built out of, and reading an
// unmarked token as "anything" would send the first GPU preset to one of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodePurpose {
    #[default]
    Cpu,
    Gpu,
    Both,
}

impl NodePurpose {
    pub fn label(self) -> &'static str {
        match self {
            NodePurpose::Cpu => "cpu",
            NodePurpose::Gpu => "gpu",
            NodePurpose::Both => "both",
        }
    }

    // Anything unrecognised is CPU, for the same reason an absent field is: the parse runs over a
    // token file an operator hand-edits, and a typo must not silently widen what a node accepts.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "gpu" => NodePurpose::Gpu,
            "both" | "any" => NodePurpose::Both,
            _ => NodePurpose::Cpu,
        }
    }

    pub fn accepts(self, hardware: PresetHardware) -> bool {
        match self {
            NodePurpose::Both => true,
            NodePurpose::Cpu => hardware == PresetHardware::Cpu,
            NodePurpose::Gpu => hardware == PresetHardware::Gpu,
        }
    }
}

// One job handed to one node. Everything the node needs to run it is in here: there is no second
// lookup, and in particular no server id to resolve against a `meta.pandora` the node does not
// have. `Job::new` already snapshots the server's preset, watermark and Drive folders at creation
// time, so this is that same snapshot travelling one hop further.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LinkJobSpec {
    pub job_id: String,
    pub job_type: String,
    pub source_kind: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_index: Option<u64>,
    // A Pancode's originating probe. The node has no such job and will not find its saved
    // `.torrent`, which is exactly why the source link travels beside it: `adopt_probe_torrent`
    // failing is survivable, an absent probe id is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_job_id: Option<String>,
    // The `.torrent` itself, for a source no link can reach: a batch whose metainfo arrived as an
    // uploaded file lives only in the coordinator's work directory, and `adopt_probe_torrent` is
    // exactly the step that cannot work on a machine with no probe job. The node writes these bytes
    // where its own downloader already looks for them, so nothing downstream of that knows the
    // difference between a torrent it fetched and one it was handed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_b64: Option<String>,
    #[serde(default)]
    pub subtitle_b64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark_b64: Option<String>,
    pub preset: String,
    pub lang: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gdrive_folder_global: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gdrive_folder_local: Option<String>,
    // Set for an HLS-only server: the node encodes and uploads nothing, because the 12-hour
    // playback capability has to be served from the one hostname that is already public. It holds
    // the finished MP4 at `Encoded`, hands it back, and the coordinator resumes from there.
    #[serde(default)]
    pub return_output: bool,
    // The originating server's Drive-only upload policy. A node holds no `meta.pandora` for that
    // guild, so without this it would publish to streaming hosts the server had switched off.
    #[serde(default)]
    pub drive_only: bool,
    // The intro group this job's server selected. The node materialises the group under its own
    // synced intro root and rebuilds the preset's concat folder from it — the coordinator's own
    // folder path means nothing on another machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intro_group: Option<String>,
    // The asset corpus this job was built against. A node that has not synced this revision
    // refuses the lease rather than encoding with whatever fonts it happens to hold.
    #[serde(default)]
    pub assets_revision: String,
    // A `/preview` job's shot list. Everything else a job needs is a field of `Job` itself; this
    // one hangs off it, so it travels as its own object rather than as six more flat fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<PreviewSpec>,
    // The keyword this job's output is kept under, and the one it continues. Allocated by the
    // coordinator before the job leaves — the keyword namespace is the coordinator's, since it is
    // what a person types into `/keycode` later — and honoured verbatim by the node, which stores
    // the file in its own keep directory under that name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<KeepSpec>,
    // The keywords a `/keycode` joins. It carries no source of its own: its inputs are whatever
    // those keywords resolve to on the machine holding them, which is the machine this job was
    // deliberately offered to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keycode: Option<KeycodeSpec>,
}

// A kept output, as the coordinator prepared it.
//
// The node must not allocate its own: a keyword is the name a person will type into `/keycode`, and
// two machines picking freely out of the same pool would hand out the same word twice and describe
// keeps nobody asked for. So the coordinator allocates, reserves the record, and sends the answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeepSpec {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    pub parent_keyword: String,
    pub output_keyword: String,
}

// What a `/keycode` joins. The node resolves these against its own keep store, where the files are
// — which is only correct because the coordinator offered this job to the node that holds them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeycodeSpec {
    pub keywords: Vec<String>,
}

// What a `/preview` needs beyond a source and a subtitle.
//
// `watermark_font` is a font *name*, never the coordinator's path to it. The coordinator resolves
// it through `find_fonts_with_roots` over `DB/fontconfig/<server>` and `DB/fontconfig/global`, both
// of which are buckets of the synced corpus, so the node runs the identical lookup over identical
// files and either finds the same font or declines the job. A path would have named a file that
// happens not to exist on the other machine, and a preview rendered in a substituted typeface is
// the same class of quiet wrongness a missing subtitle font is.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreviewSpec {
    pub shots: Vec<(u64, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark_font: Option<String>,
    #[serde(default)]
    pub ranking_log: String,
}

// Sent by the node roughly every `renew_secs`. It doubles as the liveness heartbeat and as the
// progress feed, so a node that is encoding is by definition a node that is proving it is alive.
//
// `reports` is the node's `CommData` stream forwarded verbatim — payload plus the stage transition
// it carried, in order. The coordinator replays them through `persist_side_effects` and `render`,
// which is why a remote job needs no rendering path of its own.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeaseRenew {
    pub node: String,
    #[serde(default)]
    pub worker: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reports: Vec<LinkReport>,
    // Whatever this job's logs have gained since the last renew. They land in the coordinator's own
    // log directory for the job, so `/catlogs` and the log routes answer for a remote job exactly
    // as they do for a local one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<LinkLogChunk>,
}

// A slice of one of a job's tool logs, shipped as it grows. `offset` is where it belongs in the
// file, which is what makes a re-sent chunk harmless after a renew the node never saw succeed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LinkLogChunk {
    pub name: String,
    pub offset: u64,
    pub text: String,
    // The log was replaced — a retry of the same job writing a fresh one. Start it over rather
    // than splicing new bytes onto the previous attempt's.
    #[serde(default)]
    pub reset: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LinkReport {
    pub payload: LinkPayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
}

// The renew response is the only channel the coordinator has back to a node, since a node has no
// inbound surface at all. Every out-of-band instruction rides here.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LeaseControl {
    // Carried on every renew so a font added on the coordinator reaches every working node without
    // waiting for it to finish and re-register.
    #[serde(default)]
    pub assets_revision: String,
    #[serde(default)]
    pub cancel: bool,
    // The coordinator has already reclaimed this lease (the node went quiet long enough to be
    // presumed lost) and given the job to someone else. Drop the work rather than finishing it.
    #[serde(default)]
    pub abandon: bool,
    #[serde(default)]
    pub drain: bool,
}

// The node's last word on a lease. Any reports it had not sent yet ride along, so the payload that
// carried the final links or the failure reason is never lost to a renew that never happened.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeaseResult {
    pub node: String,
    pub outcome: LinkOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reports: Vec<LinkReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkOutcome {
    Uploaded,
    // The node encoded and handed its output back instead of publishing it. Not a terminal state
    // on the coordinator: the job resumes there at `Encoded` and is uploaded locally.
    Returned,
    // A probe listed its files and is done. `Probed` is where a probe job stops even locally — it
    // then waits for someone to select a file, and the probe timeout archives it — so the node's
    // work is over while the job itself is not.
    Probed,
    Failed,
    Cancelled,
    // The node cannot run this job at all — a preset it does not have, an asset it cannot resolve.
    // Distinct from Failed because the coordinator requeues it without spending a retry.
    Declined,
}

pub fn stage_name(stage: Stage) -> String {
    stage_label(stage_to_int(stage)).to_string()
}

pub fn stage_from_name(name: &str) -> Option<Stage> {
    Some(match name {
        "Queued" => Stage::Queued,
        "Downloading" => Stage::Downloading,
        "Downloaded" => Stage::Downloaded,
        "Encoding" => Stage::Encoding,
        "Encoded" => Stage::Encoded,
        "Uploading" => Stage::Uploading,
        "Uploaded" => Stage::Uploaded,
        "Failed" => Stage::Failed,
        "Declined" => Stage::Declined,
        "Cancelled" => Stage::Cancelled,
        "Probing" => Stage::Probing,
        "Probed" => Stage::Probed,
        _ => return None,
    })
}

// The inverse of `server_effects::preset_from_name`, which is the single name table this has to
// agree with. `copy` is not in that table because no server setting or API payload selects it.
pub fn preset_name(preset: &Preset) -> String {
    match preset {
        Preset::PseudoLossless(_) => "pseudolossless",
        Preset::Dummy(_) => "dummy",
        Preset::Standard(_) => "standard",
        Preset::VerySlow(_) => "veryslow",
        Preset::Gpu(_) => "gpu",
        Preset::Av1(_) => "av1",
        Preset::Hd720(_) => "720p",
        Preset::Sd480(_) => "480p",
        Preset::Named(name, _) => return name.clone(),
        Preset::Copy => "copy",
    }
    .to_string()
}

pub fn job_type_name(job_type: JobType) -> String {
    format!("{:?}", job_type)
}

pub fn job_type_from_name(name: &str) -> Option<JobType> {
    Some(match name {
        "Encode" => JobType::Encode,
        "Pancode" => JobType::Pancode,
        "Backup" => JobType::Backup,
        "Probe" => JobType::Probe,
        _ => return None,
    })
}

pub fn source_to_wire(torrent: &TorrentType) -> (String, String) {
    (torrent.get_arg(), torrent.get())
}

pub fn source_from_wire(kind: &str, value: &str) -> Option<TorrentType> {
    let value = value.to_string();
    Some(match kind {
        "magnet" => TorrentType::Magnet(value),
        "gdrive" => TorrentType::GDrive(value),
        "direct" => TorrentType::Direct(value),
        "nomagnet" => TorrentType::Link(value),
        _ => return None,
    })
}

// A worker payload as it crosses the link. Reporting the payload itself, rather than a summary the
// coordinator would have to invert, is what lets a remote job go through `persist_side_effects` and
// `render` unchanged: the coordinator sees exactly the message the node's workers produced, and
// localises it against the job's own `lang` rather than the node's.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LinkPayload {
    pub id: String,
    // Absent for `MessagePayload::Static`, present (possibly empty) for `Progress`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
}

// Which argument of a payload is a path to a file the finished message attaches, if any.
//
// These are the three payloads `frontend::preview_done_edit` turns into a Discord attachment, and
// the path in them is a path on the machine that produced the file. That is fine while that is the
// same machine, and is the whole of what stops a node from running these jobs: the coordinator
// would attach a file that only exists somewhere else. The node uploads the artifact and rewrites
// the argument to a bare file name; the coordinator rewrites it back to a path of its own.
//
// The table mirrors `is_attachment_done` exactly, including `STUDIO_PREVIEW_DONE`, whose job is not
// leasable today — a table that covered only what happens to be leased now is one that goes quietly
// wrong the day something else is.
pub fn attachment_arg(id: &str, args: &[String]) -> Option<usize> {
    match id {
        "SUBS_DONE" => Some(1),
        "STUDIO_PREVIEW_DONE" => Some(0),
        // Two shapes: `[count, merged]` attaches, and `[count, label, path, …]` is the fallback the
        // renderer leaves as text when the merge failed. Only the first is an attachment.
        "PREVIEW_DONE" if args.len() == 2 => Some(1),
        _ => None,
    }
}

// A name that arrives off the wire and becomes a path component. Shared by the log shipper and by
// returned artifacts, because they are the same question asked about the same kind of value.
pub fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        // `Path::join("C:x")` on Windows is drive-relative and does not stay inside the directory
        // it is joined to. Nothing legitimate names a file with a colon on either platform.
        && !name.contains(':')
        && name != "."
        && name != ".."
        && !name.starts_with('.')
}

impl LinkPayload {
    pub fn from_payload(payload: &MessagePayload) -> Self {
        match payload {
            MessagePayload::Static(id) => Self { id: (*id).to_string(), args: None },
            MessagePayload::Progress(id, args) => Self {
                id: (*id).to_string(),
                args: Some(args.clone()),
            },
        }
    }

    // `None` when the node named a message this build does not have a translation for — a version
    // skew the coordinator reports rather than renders as the wrong text.
    pub fn to_payload(&self) -> Option<MessagePayload> {
        let id = intern_message_id(&self.id)?;
        Some(match &self.args {
            None => MessagePayload::Static(id),
            Some(args) => MessagePayload::Progress(id, args.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The table has to agree with `frontend::preview_done_edit`, which reads exactly these
    // arguments. Disagreeing in either direction is silent: name the wrong index and the node
    // uploads a label instead of a file, name none and the coordinator attaches a path belonging
    // to another machine.
    #[test]
    fn the_attachment_argument_is_the_one_the_renderer_reads() {
        let two = ["1".to_string(), "/n/work/subs-1.zip".to_string()];
        assert_eq!(attachment_arg("SUBS_DONE", &two), Some(1));
        assert_eq!(attachment_arg("PREVIEW_DONE", &two), Some(1));
        assert_eq!(
            attachment_arg("STUDIO_PREVIEW_DONE", &["/n/work/p.mp4".to_string()]),
            Some(0)
        );

        // A preview whose merge failed lists its shots as label/path pairs, which the renderer
        // leaves as text. There is no single attachment to return, and claiming one would rewrite
        // a label into a file name.
        let many = ["2".to_string(), "a".to_string(), "/n/a.png".to_string()];
        assert_eq!(attachment_arg("PREVIEW_DONE", &many), None);

        // Everything else is a message with no file behind it.
        assert_eq!(attachment_arg("ENCODE_PROG", &two), None);
        assert_eq!(attachment_arg("UPLOAD_DONE", &two), None);
    }

    // The name becomes a path component under a job's work directory on the coordinator, and it
    // arrives off the wire from the node.
    #[test]
    fn a_returned_artifact_cannot_name_another_directory() {
        assert!(is_plain_name("subs-4242.zip"));
        assert!(is_plain_name("output.mp4"));
        assert!(!is_plain_name("../../etc/passwd"));
        assert!(!is_plain_name("work/output.mp4"));
        assert!(!is_plain_name(".."));
        assert!(!is_plain_name("C:escaped.mp4"));
        assert!(!is_plain_name(".hidden"));
        assert!(!is_plain_name(""));
    }

    // The scheduler's whole CPU/GPU rule. A `gpu` preset reaching a CPU node does not fail
    // cleanly — ffmpeg either refuses the encoder or falls back to a software one and ships a
    // release at a quality tier nobody chose — so the match has to be exact in both directions.
    #[test]
    fn a_node_only_accepts_the_hardware_it_is_marked_for() {
        assert!(NodePurpose::Cpu.accepts(PresetHardware::Cpu));
        assert!(!NodePurpose::Cpu.accepts(PresetHardware::Gpu));
        assert!(NodePurpose::Gpu.accepts(PresetHardware::Gpu));
        // A GPU box is not a fallback for CPU work: it was marked `gpu` to keep general encoding
        // off it, and `both` is how an operator says otherwise.
        assert!(!NodePurpose::Gpu.accepts(PresetHardware::Cpu));
        assert!(NodePurpose::Both.accepts(PresetHardware::Cpu));
        assert!(NodePurpose::Both.accepts(PresetHardware::Gpu));
    }

    // Anything unrecognised narrows to CPU rather than widening, because the value is parsed out
    // of a file an operator hand-edits and a typo must not hand a node work it cannot run.
    #[test]
    fn an_unknown_or_absent_purpose_is_cpu() {
        assert_eq!(NodePurpose::default(), NodePurpose::Cpu);
        assert_eq!(NodePurpose::parse(""), NodePurpose::Cpu);
        assert_eq!(NodePurpose::parse("GPU "), NodePurpose::Gpu);
        assert_eq!(NodePurpose::parse("gpus"), NodePurpose::Cpu);
        assert_eq!(NodePurpose::parse("any"), NodePurpose::Both);
    }

    // A node from before the release fields existed still has to deserialise, and a coordinator
    // that has never synced advertises a build of zero rather than failing to answer at all.
    #[test]
    fn a_register_from_an_older_node_still_parses() {
        let older = r#"{"node":"mini-a","pandora_version":"3.5.0","encoder_identity":"x264-165-0.165.x-pandora","ffmpeg_version":"7","threads":8,"max_jobs":1,"presets":[]}"#;
        let parsed: NodeRegister = serde_json::from_str(older).unwrap();
        assert_eq!(parsed.build, 0);
        assert_eq!(parsed.migration_error, None);

        let answer: NodeRegistered =
            serde_json::from_str(r#"{"accepted":true,"renew_secs":10,"lease_timeout_secs":90}"#)
                .unwrap();
        assert_eq!(answer.purpose, NodePurpose::Cpu);
        assert_eq!(answer.release, ReleaseInfo::default());
    }

    #[test]
    fn stage_names_round_trip() {
        for stage in [
            Stage::Queued,
            Stage::Downloading,
            Stage::Downloaded,
            Stage::Encoding,
            Stage::Encoded,
            Stage::Uploading,
            Stage::Uploaded,
            Stage::Failed,
            Stage::Declined,
            Stage::Cancelled,
            Stage::Probing,
            Stage::Probed,
        ] {
            assert_eq!(stage_from_name(&stage_name(stage)), Some(stage));
        }
    }

    #[test]
    fn source_kinds_round_trip() {
        for source in [
            TorrentType::Magnet("magnet:?xt=urn:btih:abc".to_string()),
            TorrentType::Link("https://nyaa.si/download/1.torrent".to_string()),
            TorrentType::GDrive("https://drive.google.com/file/d/x".to_string()),
            TorrentType::Direct("https://host/video.mkv".to_string()),
        ] {
            let (kind, value) = source_to_wire(&source);
            let back = source_from_wire(&kind, &value).expect("wire kind is not classifiable");
            assert_eq!(back.get_arg(), kind);
            assert_eq!(back.get(), value);
        }
    }

    // The wire name a node receives has to be one `preset_from_name` accepts, or a leased job dies
    // on arrival with an unparseable preset.
    #[test]
    fn preset_names_are_accepted_by_the_name_table() {
        for preset in [
            Preset::Standard(None),
            Preset::VerySlow(None),
            Preset::Gpu(None),
            Preset::Av1(None),
            Preset::PseudoLossless(None),
            Preset::Dummy(None),
            Preset::Hd720(None),
            Preset::Sd480(None),
        ] {
            let name = preset_name(&preset);
            assert!(
                crate::pnworker::server_effects::preset_from_name(&name, None).is_some(),
                "preset name {name} is not in the shared name table",
            );
            // The same name is what the encode worker puts on `pnmpeg --preset` and what a preset
            // file is named after, so a spelling that only two of the three agree on would encode
            // at the wrong settings rather than fail.
            assert!(
                crate::lib::mpeg::preset::builtin(&name).is_some(),
                "preset name {name} resolves to no parameter table",
            );
            assert!(
                crate::lib::mpeg::preset::BUILTIN_PRESET_NAMES.contains(&name.as_str()),
                "preset name {name} is not one a node can advertise",
            );
        }
    }

    #[test]
    fn job_type_names_round_trip() {
        for job_type in [JobType::Encode, JobType::Pancode, JobType::Backup, JobType::Probe] {
            assert_eq!(job_type_from_name(&job_type_name(job_type)), Some(job_type));
        }
    }

    #[test]
    fn payloads_round_trip_through_the_wire() {
        let progress = MessagePayload::Progress(
            crate::pnworker::messages::ENCODE_PROG,
            vec!["a".to_string(), "1".to_string()],
        );
        let wire = LinkPayload::from_payload(&progress);
        match wire.to_payload().expect("known id did not intern") {
            MessagePayload::Progress(id, args) => {
                assert_eq!(id, crate::pnworker::messages::ENCODE_PROG);
                assert_eq!(args, vec!["a".to_string(), "1".to_string()]);
            }
            _ => panic!("progress payload came back static"),
        }

        let static_payload = MessagePayload::Static(crate::pnworker::messages::QUEUED);
        let wire = LinkPayload::from_payload(&static_payload);
        assert!(matches!(
            wire.to_payload().expect("known id did not intern"),
            MessagePayload::Static(id) if id == crate::pnworker::messages::QUEUED
        ));
    }

    // A node running a newer build can name a message this one has no translation for. Rendering
    // the wrong text for a real job is worse than rendering none, so it has to come back empty.
    // The outcome is the only thing distinguishing "the node published this" from "the node handed
    // it back for us to publish", and it crosses as a bare string.
    #[test]
    fn outcomes_cross_the_wire_as_their_own_names() {
        for (outcome, name) in [
            (LinkOutcome::Uploaded, "\"uploaded\""),
            (LinkOutcome::Returned, "\"returned\""),
            (LinkOutcome::Probed, "\"probed\""),
            (LinkOutcome::Failed, "\"failed\""),
            (LinkOutcome::Cancelled, "\"cancelled\""),
            (LinkOutcome::Declined, "\"declined\""),
        ] {
            let encoded = serde_json::to_string(&outcome).expect("outcome did not serialise");
            assert_eq!(encoded, name);
            let back: LinkOutcome =
                serde_json::from_str(&encoded).expect("outcome did not round trip");
            assert_eq!(back, outcome);
        }
    }

    #[test]
    fn an_unknown_message_id_does_not_intern() {
        let wire = LinkPayload { id: "NOT_A_REAL_MESSAGE".to_string(), args: None };
        assert!(wire.to_payload().is_none());
    }
}
