use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;
use crate::lib::env::core::get_pandora_env;
use crate::lib::env::standard::{PNASS, PNMPEG};
use crate::lib::mpeg::probe::ffprobe_video_height;
use crate::lib::protocol::core::Protocol;
use crate::pnworker::messages::{ENCODE_CONCAT_PROG, ENCODE_DONE, ENCODE_FAIL, ENCODE_PROG, ENCODE_START, ENCODE_WARNING, JOB_CANCELLED, MessagePayload, SERVER_EFFECTS_FAIL};
use crate::pnworker::util::{OUTPUT_RESOLUTION_FILE, ToolResult, job_cancelled, run_tool};
use crate::pnworker::tools::{PNMPEG_CONCAT, PNMPEG_ENCODE, PNMPEG_JOIN, PNMPEG_JOIN_ASS, PNMPEG_STUDIO};
use tokio::fs::rename;
use std::path::PathBuf;
use std::collections::HashMap;
use crate::pnworker::core::{Concat, KeepKind, Preset, Stage, WorkerMsg};
use crate::pnworker::util::PathValue;
use crate::pnworker::core::CommData;
// The trailing flags are `cache_resolution`, `hls_only` and `gated`: whether the job's output
// height is worth recording for the release name, whether this server publishes the release as HLS
// and nothing else — in which case the encode writes that layout itself instead of an MP4 — and
// whether this encode is background work that steps aside for `enc-main`. The coordinator decides
// the last one, because it depends on the rest of the queue as well as on the preset.
pub type EncodeData = (PathBuf, Preset, u64, Option<u64>, Option<Vec<u8>>, bool, bool, bool);
pub type StudioData = (PathBuf, PathBuf, u64);
pub type KeycodeData = (PathBuf, Vec<PathBuf>, Concat, KeepKind, u64, Option<u64>);

struct ForegroundEncodeGuard {
    path: PathBuf,
}

impl ForegroundEncodeGuard {
    fn acquire(directory: &Path, job_id: u64) -> Self {
        let root = directory.parent().unwrap_or(directory);
        let path = root.join(".foreground-encode");
        let temporary = root.join(format!(".foreground-encode.{}.tmp", std::process::id()));
        std::fs::create_dir_all(root).ok();
        if std::fs::write(&temporary, format!("{}|{job_id}\n", std::process::id())).is_ok() {
            if std::fs::rename(&temporary, &path).is_err() {
                std::fs::remove_file(&temporary).ok();
            }
        }
        Self { path }
    }
}

impl Drop for ForegroundEncodeGuard {
    fn drop(&mut self) {
        let owned = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|value| value.trim().split('|').next()?.parse::<u32>().ok())
            == Some(std::process::id());
        if owned {
            std::fs::remove_file(&self.path).ok();
        }
    }
}

#[cfg(target_os = "windows")]
use std::env;
#[cfg(target_os = "windows")]
fn path_to_ffmpeg(path: &Path) -> String {
    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let relative = path.strip_prefix(&current_dir).unwrap_or(path);
    relative.display().to_string().replace('\\', "/")
}
#[cfg(not(target_os = "windows"))]
fn path_to_ffmpeg(path: &Path) -> String {
    path.display().to_string()
}

pub async fn pn_encdeworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>, pulse: Sender<()>) {
    let mut proto = Protocol::new(vec![1]);
    let env = get_pandora_env();
    let pnmpeg_path = env.get(PNMPEG).cloned().unwrap_or_default();
    let pnass_path = env.get(PNASS).cloned().unwrap_or_default();
    'll: loop {
        let msg = tokio::select! {
            msg = rx.recv() => match msg {
                Some(msg) => msg,
                None => return,
            },
            _ = sleep(Duration::from_secs(5)) => {
                pulse.try_send(()).ok();
                continue;
            }
        };
        if let WorkerMsg::Studio((directory, manifest, job_id)) = msg {
                let _foreground = ForegroundEncodeGuard::acquire(&directory, job_id);
                run_studio_job(&pnmpeg_path, directory, manifest, job_id, &mut proto, &tx).await;
                continue 'll;
            }
            if let WorkerMsg::Keycode((directory, inputs, concat, kind, job_id, _server_id)) = msg {
                let _foreground = ForegroundEncodeGuard::acquire(&directory, job_id);
                let Some(first) = inputs.first() else {
                    tx.send((job_id, MessagePayload::Static(ENCODE_FAIL), Some(Stage::Failed))).await.unwrap();
                    continue 'll;
                };
                let rest = inputs.iter().skip(1).map(|p| path_to_ffmpeg(p)).collect::<Vec<_>>();
                let (spec, mode) = match kind {
                    KeepKind::Encode => (PNMPEG_JOIN, "--joinconcat"),
                    KeepKind::Backup => (PNMPEG_JOIN_ASS, "--joinass"),
                };
                let mut params = HashMap::from([
                    ("INPUT", PathValue::from(path_to_ffmpeg(first))),
                    ("OUTPUT", PathValue::from(path_to_ffmpeg(directory.join("work").join("output.mp4").as_path()))),
                    ("CANDIDATES", PathValue::from(rest)),
                    ("INTRO_DIR", PathValue::from(concat.intro.unwrap_or_default())),
                    ("OUTRO_DIR", PathValue::from(concat.outro.unwrap_or_default())),
                    ("MODE", PathValue::from(mode.to_string())),
                    ("NEGKEY", PathValue::from("pn-encode-main".to_string())),
                    ("CANCELFILE", PathValue::from(directory.join("CANCEL").display().to_string())),
                    ("LOGFILE", PathValue::from(directory.join("log").join(format!("PNmpeg_Keycode{}.log", job_id)).display().to_string())),
                ]);
                if kind == KeepKind::Backup {
                    params.insert("ASS", PathValue::from(path_to_ffmpeg(directory.join("contents").join("subtitle.ass").as_path())));
                }
                if job_cancelled(&directory) {
                    tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.unwrap();
                    continue 'll;
                }
                tx.send((job_id, MessagePayload::Static(ENCODE_START), Some(Stage::Encoding))).await.ok();
                let result = run_tool(
                    &pnmpeg_path,
                    spec,
                    &params,
                    job_id,
                    &mut proto,
                    |data| keycode_progress(data, job_id, &tx),
                ).await;
                match result {
                    ToolResult::Success => {
                        tx.send((job_id, MessagePayload::Static(ENCODE_DONE), Some(Stage::Encoded))).await.unwrap();
                    }
                    ToolResult::Fail => {
                        tx.send((job_id, MessagePayload::Static(ENCODE_FAIL), Some(Stage::Failed))).await.unwrap();
                    }
                    ToolResult::Cancel => {
                        tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.unwrap();
                    }
                }
                continue 'll;
            }
            let WorkerMsg::Encode((directory, preset, job_id, server_id, watermark, cache_resolution, hls_only, idle_encode)) = msg else {
                continue 'll;
            };
            // A background encode does not take the foreground marker: the marker is what every
            // idle encoder waits on, and one that published it would be telling itself the machine
            // is busy. Not holding it is also what leaves `enc-main` free to start the encode this
            // one is supposed to step aside for.
            let _foreground = (!idle_encode)
                .then(|| ForegroundEncodeGuard::acquire(&directory, job_id));
            let mut resolution_probe = if cache_resolution {
                let input = directory
                    .join("contents")
                    .join("torrent")
                    .join("input.mkv")
                    .display()
                    .to_string();
                Some(tokio::task::spawn_blocking(move || ffprobe_video_height(&input)))
            } else {
                None
            };
            // The canonical preset names, which are also the file names under
            // `DB/config/global/presets/`: pnmpeg takes `--preset <name>` and looks a file up
            // before falling back to its built-in table, so a preset an operator edited applies
            // here without the worker knowing anything about it.
            let (concat, insert) = match preset {
                Preset::PseudoLossless(cc) => (cc, "pseudolossless".to_string()),
                Preset::Gpu(cc)            => (cc, "gpu".to_string()),
                Preset::Av1(cc)            => (cc, "av1".to_string()),
                Preset::Standard(cc)       => (cc, "standard".to_string()),
                Preset::VerySlow(cc)       => (cc, "veryslow".to_string()),
                Preset::Dummy(cc)          => (cc, "dummy".to_string()),
                Preset::Hd720(cc)          => (cc, "720p".to_string()),
                Preset::Sd480(cc)          => (cc, "480p".to_string()),
                // A preset that is only a file is named by that file, which is the same name
                // pnmpeg looks it up by. Nothing here needs to know anything else about it.
                Preset::Named(name, cc)    => (cc, name),
                Preset::Copy               => (Concat::NONE, "copy".to_string()),
            };
            let Concat { intro: intro_dir, outro: outro_dir } = concat;
            // The release name is taken from the source height, which a downscaling preset is
            // about to undercut.
            let height_cap = match insert.as_str() {
                "720p" => Some(720),
                "480p" => Some(480),
                _ => None,
            };
            // One concat pass stitches on whichever of the two the server configured, so the
            // progress line counts two runs for an intro, an outro, or both.
            let concats = intro_dir.is_some() || outro_dir.is_some();
            let intro_q = if concats { 2 } else { 1 };
            // Whichever ffmpeg run is this job's last one writes the HLS layout itself, so the
            // broker never has to take an MP4 apart into the same chunks. With a concat that is the
            // concat rather than the encode: the encode still has to leave a file the concat can
            // read back.
            let hls_direct = hls_only && !concats;
            let hls_directory = directory.join("work").join("hls");
            // The names this server publishes under. Read once per job, because the layout has to
            // be spelled the same way by whichever run ends up writing it.
            let hls_name = if hls_only {
                crate::pnworker::server_config::server_hls_name(server_id).await
            } else {
                String::new()
            };
            // A retry of this job may have left a layout from the attempt before it; the upload
            // worker adopts whatever is here, so it cannot be allowed to outlive its encode.
            tokio::fs::remove_dir_all(&hls_directory).await.ok();
            let fontconfig_dir = PathBuf::from("DB").join("fontconfig").join(
                server_id.map(|id| id.to_string()).unwrap_or_else(|| "global".to_string())
            );
            tokio::fs::create_dir_all(&fontconfig_dir).await.ok();

            if job_cancelled(&directory) {
                tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.unwrap();
                continue 'll;
            }
            let effects = match crate::pnworker::server_effects::server_effects(
                &directory,
                watermark.as_deref(),
                &pnass_path,
                job_id,
            ).await {
                Ok(effects) => effects,
                Err(e) if e == "cancelled" => {
                    tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.unwrap();
                    continue 'll;
                }
                Err(e) => {
                    tx.send((job_id, MessagePayload::Progress(SERVER_EFFECTS_FAIL, vec![e]), Some(Stage::Failed))).await.unwrap();
                    continue 'll;
                }
            };
            for warning in effects.warnings {
                tx.try_send((job_id, MessagePayload::Progress(ENCODE_WARNING, vec![warning]), None)).ok();
            }
            tx.send((job_id, MessagePayload::Static(ENCODE_START), Some(Stage::Encoding))).await.ok();
            let mut encode_params = HashMap::from([
                    ("INPUT",      PathValue::from(path_to_ffmpeg(directory.join("contents").join("torrent").join("input.mkv").as_path()))),
                    ("OUTPUT",     PathValue::from(path_to_ffmpeg(directory.join("work").join("output_noconcat.mp4").as_path()))),
                    ("ASS",        PathValue::from(path_to_ffmpeg(effects.subtitle.as_path()))),
                    ("FONTCONFIG", PathValue::from(path_to_ffmpeg(fontconfig_dir.as_path()))),
                    ("PRESET",     PathValue::from(insert.clone())),
                    ("NEGKEY",     PathValue::from("pn-encode-main".to_string())),
                    ("CANCELFILE", PathValue::from(directory.join("CANCEL").display().to_string())),
                    ("LOGFILE",    PathValue::from(directory.join("log").join(format!("PNmpeg_Encode{}.log", job_id)).display().to_string())),
            ]);
            if hls_direct {
                encode_params.insert("HLS", PathValue::from(path_to_ffmpeg(hls_directory.as_path())));
                encode_params.insert("HLSNAME", PathValue::from(hls_name.clone()));
            }
            // The two files a background encode gates itself on, and the same two the speculative
            // planners have always used: the marker that says an ordinary encode is running, and
            // the lease that keeps one idle consumer on the machine at a time. Passing them is what
            // turns the ordinary encode into a gated one; pnmpeg reads the preset for the rest.
            if idle_encode {
                let worker_root = directory.parent().unwrap_or(&directory);
                encode_params.insert(
                    "AOTBUSY",
                    PathValue::from(worker_root.join(".foreground-encode").display().to_string()),
                );
                encode_params.insert(
                    "AOTLOCK",
                    PathValue::from(worker_root.join(".aot-owner").display().to_string()),
                );
                // Only so the gated encoder's own log lines name the job they belong to. A
                // background encode never publishes the marker, so it can never be the owner the
                // gate reads back and waves through.
                encode_params.insert("AOTJOBID", PathValue::from(job_id.to_string()));
            }
            let result = run_tool(
                &pnmpeg_path,
                PNMPEG_ENCODE,
                &encode_params,
                job_id,
                &mut proto,
                |data| {
                    let out: u16 = match data.get(0).and_then(|v| v.parse()) {
                        Some(v) => v,
                        None => return None,
                    };
                    match out {
                        0 => {
                            let payload = data.get(1).and_then(|v| v.as_multi())?;
                            let fps       = payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                            let frame     = payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                            let totlframe = payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
                            let bitrate   = payload.get(3).and_then(|v| v.as_str()).unwrap_or("0");
                            tx.try_send((job_id, MessagePayload::Progress(ENCODE_PROG, vec![
                                intro_q.to_string(),
                                frame.to_string(),
                                totlframe.to_string(),
                                fps.to_string(),
                                bitrate.to_string(),
                            ]), None)).ok();
                        }
                        1 => return Some(ToolResult::Success),
                        2 => return Some(ToolResult::Fail),
                        3 => return Some(ToolResult::Cancel),
                        4 => {
                            if let Some(warning) = data.get(1).and_then(|v| v.as_str()) {
                                tx.try_send((job_id, MessagePayload::Progress(ENCODE_WARNING, vec![
                                    warning.to_string(),
                                ]), None)).ok();
                            }
                        }
                        _ => {}
                    }
                    None
                },
            ).await;

            match result {
                ToolResult::Fail => {
                    tx.send((job_id, MessagePayload::Static(ENCODE_FAIL), Some(Stage::Failed))).await.unwrap();
                    continue 'll;
                }
                ToolResult::Cancel => {
                    tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.unwrap();
                    continue 'll;
                }
                ToolResult::Success => {}
            }

            if concats {
                if job_cancelled(&directory) {
                    tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.unwrap();
                    continue 'll;
                }
                // Both folders go to the same run. pnmpeg writes one list file holding the intro,
                // the encode, and the outro in that order, so an episode with both is one
                // stream-copy mux rather than two passes over the same bytes. An empty path is how
                // it is told a side has no group, which is what `unwrap_or_default` produces.
                let mut concat_params = HashMap::from([
                        ("INPUT",      PathValue::from(path_to_ffmpeg(directory.join("work").join("output_noconcat.mp4").as_path()))),
                        ("OUTPUT",     PathValue::from(path_to_ffmpeg(directory.join("work").join("output.mp4").as_path()))),
                        ("INTRO_DIR",  PathValue::from(intro_dir.as_deref().map(|dir| path_to_ffmpeg(Path::new(dir))).unwrap_or_default())),
                        ("OUTRO_DIR",  PathValue::from(outro_dir.as_deref().map(|dir| path_to_ffmpeg(Path::new(dir))).unwrap_or_default())),
                        ("PRESET",     PathValue::from(insert.clone())),
                        ("NEGKEY",     PathValue::from("pn-encode-main".to_string())),
                        ("CANCELFILE", PathValue::from(directory.join("CANCEL").display().to_string())),
                        ("LOGFILE",    PathValue::from(directory.join("log").join(format!("PNmpeg_Concat{}.log", job_id)).display().to_string())),
                ]);
                if hls_only {
                    concat_params.insert("HLS", PathValue::from(path_to_ffmpeg(hls_directory.as_path())));
                    concat_params.insert("HLSNAME", PathValue::from(hls_name.clone()));
                }
                let result = run_tool(
                    &pnmpeg_path,
                    PNMPEG_CONCAT,
                    &concat_params,
                    job_id,
                    &mut proto,
                    |data| {
                        let out: u16 = match data.get(0).and_then(|v| v.parse()) {
                            Some(v) => v,
                            None => return None,
                        };
                        match out {
                            0 => {
                                let payload = data.get(1).and_then(|v| v.as_multi())?;
                                let fps       = payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
                                let frame     = payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
                                let totlframe = payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
                                let bitrate   = payload.get(3).and_then(|v| v.as_str()).unwrap_or("0");
                            tx.try_send((job_id, MessagePayload::Progress(ENCODE_CONCAT_PROG, vec![
                                frame.to_string(),
                                totlframe.to_string(),
                                fps.to_string(),
                                bitrate.to_string(),
                            ]), None)).ok();
                            }
                            1 => return Some(ToolResult::Success),
                            2 => return Some(ToolResult::Fail),
                            3 => return Some(ToolResult::Cancel),
                            4 => {
                                if let Some(warning) = data.get(1).and_then(|v| v.as_str()) {
                                    tx.try_send((job_id, MessagePayload::Progress(ENCODE_WARNING, vec![
                                        warning.to_string(),
                                    ]), None)).ok();
                                }
                            }
                            _ => {}
                        }
                        None
                    },
                ).await;

                match result {
                    ToolResult::Success => {
                        persist_output_resolution(&directory, resolution_probe.take(), height_cap).await;
                        tx.send((job_id, MessagePayload::Static(ENCODE_DONE), Some(Stage::Encoded))).await.unwrap();
                    }
                    ToolResult::Fail => {
                        tx.send((job_id, MessagePayload::Static(ENCODE_FAIL), Some(Stage::Failed))).await.unwrap();
                    }
                    ToolResult::Cancel => {
                        tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.unwrap();
                    }
                }
            } else {
                // An encode that muxed its own HLS has no MP4 to promote — the playlist and chunks
                // in `work/hls` are the output, and the upload worker publishes them from there.
                let encoded = directory.join("work").join("output_noconcat.mp4");
                if encoded.exists() {
                    rename(encoded, directory.join("work").join("output.mp4")).await.unwrap();
                }
                persist_output_resolution(&directory, resolution_probe.take(), height_cap).await;
                tx.send((job_id, MessagePayload::Static(ENCODE_DONE), Some(Stage::Encoded))).await.unwrap();
            }
        println!("[Pandora Encoder] End of Session");
    }
}

async fn persist_output_resolution(
    directory: &Path,
    probe: Option<tokio::task::JoinHandle<Option<u32>>>,
    height_cap: Option<u32>,
) {
    let Some(probe) = probe else {
        return;
    };
    let Ok(Ok(Some(height))) = tokio::time::timeout(Duration::from_secs(5), probe).await else {
        return;
    };
    let height = height_cap.map(|cap| height.min(cap)).unwrap_or(height);
    let path = directory.join("work").join(OUTPUT_RESOLUTION_FILE);
    if let Err(e) = tokio::fs::write(&path, format!("{}p", height)).await {
        eprintln!(
            "[Pandora Encoder] failed to cache output resolution at {}: {}",
            path.display(),
            e,
        );
    }
}

async fn run_studio_job(
    pnmpeg_path: &str,
    directory: PathBuf,
    manifest: PathBuf,
    job_id: u64,
    proto: &mut Protocol,
    tx: &Sender<CommData>,
) {
    let output = directory.join("work").join("output.mp4");
    if job_cancelled(&directory) {
        tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.ok();
        return;
    }
    tx.send((job_id, MessagePayload::Static(ENCODE_START), Some(Stage::Encoding))).await.ok();
    let result = run_tool(
        pnmpeg_path,
        PNMPEG_STUDIO,
        &HashMap::from([
            ("MANIFEST", PathValue::from(manifest.display().to_string())),
            ("OUTPUT", PathValue::from(output.display().to_string())),
            ("NEGKEY", PathValue::from("pn-encode-main".to_string())),
            ("CANCELFILE", PathValue::from(directory.join("CANCEL").display().to_string())),
            ("LOGFILE", PathValue::from(directory.join("log").join(format!("PNmpeg_Studio{}.log", job_id)).display().to_string())),
        ]),
        job_id,
        proto,
        |data| studio_progress(data, job_id, tx),
    ).await;
    match result {
        ToolResult::Success => tx.send((job_id, MessagePayload::Static(ENCODE_DONE), Some(Stage::Encoded))).await.ok(),
        ToolResult::Fail => tx.send((job_id, MessagePayload::Static(ENCODE_FAIL), Some(Stage::Failed))).await.ok(),
        ToolResult::Cancel => tx.send((job_id, MessagePayload::Static(JOB_CANCELLED), Some(Stage::Cancelled))).await.ok(),
    };
}

fn studio_progress(
    data: &crate::lib::protocol::core::TypeC,
    job_id: u64,
    tx: &Sender<CommData>,
) -> Option<ToolResult> {
    let out = data.get(0).and_then(|v| v.parse::<u16>())?;
    match out {
        0 => {
            let payload = data.get(1).and_then(|v| v.as_multi())?;
            tx.try_send((job_id, MessagePayload::Progress(ENCODE_PROG, vec![
                "1".to_string(),
                payload.get(1).and_then(|v| v.as_str()).unwrap_or("0").to_string(),
                payload.get(2).and_then(|v| v.as_str()).unwrap_or("0").to_string(),
                payload.get(0).and_then(|v| v.as_str()).unwrap_or("0").to_string(),
                payload.get(3).and_then(|v| v.as_str()).unwrap_or("0").to_string(),
            ]), None)).ok();
        }
        1 => return Some(ToolResult::Success),
        2 => return Some(ToolResult::Fail),
        3 => return Some(ToolResult::Cancel),
        4 => {
            if let Some(warning) = data.get(1).and_then(|v| v.as_str()) {
                tx.try_send((job_id, MessagePayload::Progress(ENCODE_WARNING, vec![warning.to_string()]), None)).ok();
            }
        }
        _ => {}
    }
    None
}

fn keycode_progress(
    data: &crate::lib::protocol::core::TypeC,
    job_id: u64,
    tx: &Sender<CommData>,
) -> Option<ToolResult> {
    let out: u16 = match data.get(0).and_then(|v| v.parse()) {
        Some(v) => v,
        None => return None,
    };
    match out {
        0 => {
            let payload = data.get(1).and_then(|v| v.as_multi())?;
            let fps = payload.get(0).and_then(|v| v.as_str()).unwrap_or("0");
            let frame = payload.get(1).and_then(|v| v.as_str()).unwrap_or("0");
            let totlframe = payload.get(2).and_then(|v| v.as_str()).unwrap_or("0");
            let bitrate = payload.get(3).and_then(|v| v.as_str()).unwrap_or("0");
            tx.try_send((
                job_id,
                MessagePayload::Progress(
                    ENCODE_CONCAT_PROG,
                    vec![
                        frame.to_string(),
                        totlframe.to_string(),
                        fps.to_string(),
                        bitrate.to_string(),
                    ],
                ),
                None,
            ))
            .ok();
        }
        1 => return Some(ToolResult::Success),
        2 => return Some(ToolResult::Fail),
        3 => return Some(ToolResult::Cancel),
        4 => {
            if let Some(warning) = data.get(1).and_then(|v| v.as_str()) {
                tx.try_send((
                    job_id,
                    MessagePayload::Progress(ENCODE_WARNING, vec![warning.to_string()]),
                    None,
                ))
                .ok();
            }
        }
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_guard_publishes_and_removes_its_live_pid() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pn-foreground-{nonce}"));
        let job = root.join("123");
        std::fs::create_dir_all(&job).unwrap();
        let marker = root.join(".foreground-encode");
        {
            let _guard = ForegroundEncodeGuard::acquire(&job, 123);
            assert_eq!(
                std::fs::read_to_string(&marker).unwrap().trim(),
                format!("{}|123", std::process::id()),
            );
        }
        assert!(!marker.exists());
        std::fs::remove_dir_all(root).ok();
    }
}
