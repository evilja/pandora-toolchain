use pandora_toolchain::lib::mpeg::{
    core::{
        FFmpeg, FfmpegParams, do_comm_encode_ffmpeg}, preset::{
        CONCAT, CONCAT_LEGACY, ResolvedPreset, resolve as resolve_preset
    }, probe::{
        ConcatMedia, ffprobe_concat_media, ffprobe_estimated_frames, ffprobe_frame,
        ffprobe_framerate, ffprobe_lang,
        ffprobe_samplerate
    }
};
use tokio::{fs::File, io::AsyncWriteExt, time::{Duration, Instant}};
use pandora_toolchain::{pn_data, pn_emit, pn_schema};
use pandora_toolchain::lib::mpeg::core::RpbData;
use pandora_toolchain::lib::bin::resolve_runtime_binary;
use pandora_toolchain::lib::mpeg::hls::HlsNames;
use pandora_toolchain::lib::mpeg::probe::ffprobe_video_height;
use pandora_toolchain::lib::secret::random_uuid_v4;
use pandora_toolchain::lib::logging::diag::{exit_reason, memory_line, process_rss_mib, tail_line};
use pandora_toolchain::lib::logging::tool::ToolLog;
use pandora_toolchain::lib::mpeg::studio::{studio_ffmpeg_params, write_ffconcat, StudioRenderManifest};
use pandora_toolchain::lib::mpeg::subs::{ExtractOutcome, extract_subtitle, ffprobe_subtitle_streams};
use pandora_toolchain::lib::protocol::core::{Protocol, Schema, ToolInfo};
use std::str::FromStr;
use clap::Parser;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use std::borrow::Cow;

#[derive(Parser, Debug)]
#[command(
    name = "pnmpeg",
    version = "0.1.1",
    about = "Pandora Toolchain FFmpeg wrapper",
    long_about = None
)]
struct Args {
    /// Use GPU encoder (nvenc / vaapi / mesa)
    #[arg(long)]
    gpu: bool,

    /// Use x264 software encoder
    #[arg(long)]
    x264: bool,

    #[arg(long)]
    pseudolossless: bool,

    /// x264 veryslow at CRF 18 with untouched x264 defaults
    #[arg(long)]
    veryslow: bool,

    /// The x264 preset downscaled to at most 720 lines
    #[arg(long = "720p")]
    p720: bool,

    /// The x264 preset downscaled to at most 480 lines
    #[arg(long = "480p")]
    p480: bool,

    /// Encode with the named preset, from `DB/config/global/presets/<name>.toml` if it exists and
    /// the built-in table otherwise. The equivalent of the flags above, and the only way to reach
    /// a preset that exists solely as a file.
    #[arg(long)]
    preset: Option<String>,

    #[arg(long)]
    concat: bool,

    #[arg(long)]
    dummy: bool,

    /// Render a serialized Pandora Studio manifest.
    #[arg(long)]
    studio: bool,

    /// Plan natural IDR boundaries from a growing download-prefix sidecar.
    #[arg(long)]
    plan_prefix: bool,

    /// Continuously encode a growing prefix with one persistent x264 instance.
    #[arg(long, hide = true)]
    linear_prefix: bool,

    #[arg(long, hide = true)]
    aot_job_id: Option<u64>,

    /// Internal: encode one speculative natural-IDR chunk.
    #[arg(long, hide = true)]
    aot_chunk: bool,

    #[arg(long, hide = true)]
    aot_stopfile: Option<String>,

    #[arg(long, hide = true)]
    aot_busyfile: Option<String>,

    #[arg(long, hide = true)]
    aot_lockfile: Option<String>,

    #[arg(long, hide = true)]
    start_frame: Option<u64>,

    #[arg(long, hide = true)]
    frame_count: Option<u64>,

    #[arg(long, hide = true)]
    video_width: Option<u32>,

    #[arg(long, hide = true)]
    video_height: Option<u32>,

    #[arg(long, hide = true)]
    fps_num: Option<u32>,

    #[arg(long, hide = true)]
    fps_den: Option<u32>,

    #[arg(long, hide = true)]
    aot_crf: Option<f32>,

    #[arg(long, hide = true)]
    aot_preset: Option<String>,

    #[arg(long, hide = true)]
    aot_tune: Option<String>,

    #[arg(long, hide = true)]
    aot_profile: Option<String>,

    #[arg(long, hide = true)]
    aot_level: Option<String>,

    #[arg(long, hide = true)]
    aot_x264_params: Option<String>,

    /// Extract every text subtitle track of --input into the --output directory.
    #[arg(long)]
    extractsubs: bool,

    #[arg(long)]
    legacyconcat: bool,

    #[arg(long)]
    joinconcat: bool,

    #[arg(long)]
    joinass: bool,

    /// Input file
    #[arg(short, long)]
    input: String,

    /// Output file
    #[arg(short, long)]
    output: String,

    /// ASS subtitle file
    #[arg(short, long)]
    ass: Option<String>,

    #[arg(long, alias = "fontdir")]
    fontconfig: Option<String>,

    /// Language to search in input file
    #[arg(short, long)]
    lang: Option<String>,

    #[arg(short, long)]
    subinput: Option<String>,

    /// Additional inputs for join modes, or legacy individual intro candidates.
    #[arg(short, long, num_args = 0..)]
    candidate: Vec<String>,

    /// Folder containing the retained variants for one intro group.
    #[arg(long)]
    intro_dir: Option<String>,

    #[arg(long)]
    negkey: Option<String>,

    #[arg(long)]
    negotiator: Option<String>,

    #[arg(long)]
    negver: Option<String>,

    /// Write the final video/audio mux as HLS into this directory instead of an MP4 at --output.
    #[arg(long)]
    hls: Option<String>,

    #[arg(long)]
    cancelfile: Option<String>,

    #[arg(long)]
    logfile: Option<String>,
}

#[inline]
fn wrap(a: &str) -> String { return String::from(a) }

// The frame height the selected preset caps its output at, if it caps one at all. It is a cap and
// not a target: the scale filters never upscale a source that is already smaller than this. Read
// out of the preset's own filter chain, so a preset file that scales is named after what it
// actually produced rather than after the flag it was reached by.
fn scale_height(preset: Option<&ResolvedPreset>) -> Option<u32> {
    preset.and_then(ResolvedPreset::scale_height)
}

// The settings the ahead-of-time encoder drives libx264 with.
//
// These come from the selected preset rather than from a table of their own. The speculative prefix
// and the foreground encode have to agree exactly — the output file is one half of each — and a
// second copy of the numbers could only ever agree with the built-ins. A preset file would have
// left the prefix at the compiled CRF and the rest of the episode at the configured one, in one
// file, with nothing downstream able to tell.
// Whether this run may adopt the prefix the download worker encoded speculatively, and whether it
// splits the episode across parallel encoders instead. Both are named rather than inlined at the
// branch so a test can hold them to the same answers the boolean flags used to give — the whole of
// this regression was four readings of the flags drifting apart from the preset actually in use.
fn adopts_linear_prefix(preset: Option<&ResolvedPreset>) -> bool {
    preset.is_some_and(|preset| preset.wants_linear_aot() && !preset.wants_chunked_encode())
}

fn encodes_in_chunks(preset: Option<&ResolvedPreset>) -> bool {
    preset.is_some_and(ResolvedPreset::wants_chunked_encode)
}

fn planner_encoder_config(preset: Option<&ResolvedPreset>) -> pnx264::Config {
    let settings = preset.map(ResolvedPreset::x264_settings).unwrap_or_default();
    pnx264::Config {
        crf: settings.crf.map(f32::from).unwrap_or(17.0),
        threads: std::env::var("PLAN_THREADS").ok().and_then(|value| value.parse().ok()).unwrap_or(0),
        preset: settings.preset.or_else(|| Some("fast".to_string())),
        tune: settings.tune,
        profile: settings.profile.or_else(|| Some("high".to_string())),
        level: settings.level.or_else(|| Some("4.1".to_string())),
        x264_params: settings.x264_params,
        plan_only: true,
        ..Default::default()
    }
}



fn media_bitrate_kbps(bytes: u64, media_micros: u64) -> u64 {
    if bytes == 0 || media_micros == 0 {
        return 0;
    }
    let kbps = u128::from(bytes) * 8 * 1000 / u128::from(media_micros);
    kbps.min(u128::from(u64::MAX)) as u64
}

// A frame total for the handoff to count against. The input itself is the obvious source and is
// often the one thing that cannot be read: while the speculative encoder holds the downloaded file
// open, the production bind mount stops resolving the name it was renamed to, so every probe of it
// fails and the progress the user watches has no denominator. The download worker records the count
// while that path still works, and this reads it back.
fn handoff_frame_total(input: &str, parent: &Path) -> (Option<u64>, &'static str) {
    if let Some(frames) = ffprobe_estimated_frames(input) {
        return (Some(frames), "the input header");
    }
    let recorded = std::fs::read_to_string(parent.join(TOTAL_FRAMES_SIDECAR))
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .filter(|frames| *frames != 0);
    (recorded, "the download worker")
}

// How far the header estimate may sit from what the speculative encoder actually produced before
// the exact count is worth a full demux. Container duration rounding is worth a frame either way; a
// truncated encode — the thing being guarded against — is short by thousands.
const TOTAL_ESTIMATE_TOLERANCE: u64 = 2;

// Where the download worker leaves the frame count of the file it just finished, beside the job's
// scratch. See `handoff_frame_total`.
const TOTAL_FRAMES_SIDECAR: &str = "total_frames";

// How many half-second attempts the audio retry gets to see the input reappear.
const AUDIO_RETRY_ATTEMPTS: u32 = 20;

// How long the linear AOT state file may be missing before the handoff stops believing in it. The
// speculative encoder rewrites it roughly once a second, so anything shorter-lived than this is the
// publisher's rename window rather than a file that has actually gone away.
const MISSING_STATE_GRACE: Duration = Duration::from_secs(15);

// What a directory still holds, for a log line written at the moment something has gone missing
// from it. Names only, capped: the point is to tell an emptied directory from a wiped one.
fn directory_names(directory: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return "unreadable".to_string();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .take(12)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    if names.is_empty() {
        return "empty".to_string();
    }
    names.sort();
    names.join(",")
}

// The chunk directory an HLS run writes into, and the names it will use. The height is read off
// whatever this run is about to copy or encode, and the broker re-derives the rest of the layout
// from the playlist it finds; ffmpeg will not create the directory its segment pattern points into,
// so that happens here. An earlier attempt at the same job may have left a layout behind, and
// publishing a mix of the two would serve chunks from both encodes.
fn prepare_hls_output(
    directory: &Path,
    height_source: &str,
    height_cap: Option<u32>,
) -> Result<HlsNames, String> {
    let id = random_uuid_v4().map_err(|e| format!("HLS output has no entropy: {e}"))?;
    // A downscaling preset is about to write fewer lines than the source it was measured on, and
    // the playlist name is what the player reads the variant's resolution off.
    let height = match (ffprobe_video_height(height_source), height_cap) {
        (Some(height), Some(cap)) => Some(height.min(cap)),
        (Some(height), None) => Some(height),
        (None, cap) => cap,
    };
    let names = HlsNames::new(height, &id);
    std::fs::remove_dir_all(directory).ok();
    std::fs::create_dir_all(directory.join(&names.chunk_directory))
        .map_err(|e| format!("HLS directory {} could not be created: {e}", directory.display()))?;
    Ok(names)
}

// Rewrite a preset that writes one MP4 into one that writes the HLS layout: `+faststart` belongs to
// the MP4 muxer and is refused by any other, and the output filename becomes the options that name
// the playlist and its chunks. Every preset ends in its output, which is the argument being
// replaced — the encode's, and the intro concat's, which is a stream copy and just as final a mux.
fn retarget_params_to_hls(
    params: &mut Vec<FfmpegParams>,
    directory: &Path,
    height_source: &str,
    height_cap: Option<u32>,
    log: &mut ToolLog,
) -> Result<(), String> {
    let names = prepare_hls_output(directory, height_source, height_cap)?;
    params.retain(|param| !matches!(param, FfmpegParams::Movflags));
    let output = params
        .iter()
        .position(|param| matches!(param, FfmpegParams::Output(_)))
        .ok_or("this ffmpeg preset has no output to write HLS to")?;
    params[output] = FfmpegParams::Passthrough(names.muxer_args_in(directory));
    log.line(&format!(
        "writing HLS into {}: {} pointing at {}",
        directory.display(),
        names.media,
        names.chunk_directory
    ));
    Ok(())
}

// The AOT final mux, written as HLS instead of an MP4.
fn mux_linear_aot_hls(
    video: &Path,
    audio: &Path,
    directory: &Path,
    mux_errors: &Path,
    log: &mut ToolLog,
) -> Result<String, String> {
    let names = prepare_hls_output(directory, &video.display().to_string(), None)?;
    let status = Command::new(resolve_runtime_binary("ffmpeg"))
        .args(["-v", "error", "-i"])
        .arg(video)
        .args(["-i"])
        .arg(audio)
        .args(["-map", "0:v:0", "-map", "1:a:0", "-c", "copy", "-y"])
        .args(names.muxer_args_in(directory))
        .stderr(std::fs::File::create(mux_errors).map(Stdio::from).unwrap_or_else(|_| Stdio::null()))
        .status()
        .map_err(|e| format!("spawn linear AOT HLS mux: {e}"))?;
    if !status.success() {
        std::fs::remove_dir_all(directory).ok();
        return Err(format!(
            "linear AOT HLS mux failed: {}; ffmpeg said: {}",
            exit_reason(&status),
            tail_line(mux_errors, 3)
        ));
    }
    log.line(&format!(
        "linear AOT muxed HLS into {}: {} pointing at {}",
        directory.display(),
        names.media,
        names.chunk_directory
    ));
    Ok(directory.join(&names.media).display().to_string())
}

fn finish_linear_aot(
    args: &Args,
    preset: &ResolvedPreset,
    audio_index: &str,
    proto: &Protocol,
    neg: &str,
    log: &mut ToolLog,
) -> Result<bool, String> {
    let output = PathBuf::from(&args.output);
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let state_path = parent.join("linear-aot.state");
    let aot_video = parent.join("linear-aot-video.mp4");
    let initial = match pnx264::linear::LinearAotState::read(&state_path) {
        Ok(state) => state,
        Err(e) => {
            // Every Standard/PseudoLossless/Dummy encode passes through here, most of them with no
            // speculation to adopt. Saying so is what separates "AOT was never started for this
            // job" from "AOT ran and its handoff was refused".
            log.line(&format!(
                "no linear AOT handoff at {} ({e}); encoding linearly from the start",
                state_path.display()
            ));
            return Ok(false);
        }
    };
    log.line(&format!(
        "linear AOT handoff found: complete={} pid={} job={} frames={} bytes={} {}",
        initial.complete, initial.pid, initial.job_id, initial.frames, initial.bytes, memory_line()
    ));
    let wanted = preset.aot_compatibility();
    if initial.compatibility != wanted {
        // The compatibility string is separated by a control character so that an argument
        // containing a space cannot forge a boundary in it. That makes it unreadable in a log, and
        // this line is the one place a person reads it, so it is spaced out again here.
        let readable = |value: &str| value.replace('\u{1f}', " ");
        log.line(&format!(
            "linear AOT is incompatible (speculated {:?}, this encode wants {:?}); using established linear encode",
            readable(&initial.compatibility), readable(&wanted)
        ));
        std::fs::remove_file(&state_path).ok();
        std::fs::remove_file(&aot_video).ok();
        return Ok(false);
    }
    if !initial.complete && !initial.process_alive() {
        // The speculative encoder died before it could finish and before this process arrived.
        // Nothing in it writes a message on the way out, so this line and the memory reading beside
        // it are the only evidence that will exist afterwards.
        log.line(&format!(
            "linear AOT process {} is gone with {} frames and {} bytes encoded and its state still incomplete; using established linear encode — {}",
            initial.pid, initial.frames, initial.bytes, memory_line()
        ));
        std::fs::remove_file(&state_path).ok();
        std::fs::remove_file(&aot_video).ok();
        return Ok(false);
    }

    let scratch = parent.join("pnmpeg-linear-aot");
    std::fs::remove_dir_all(&scratch).ok();
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let audio = scratch.join("audio.m4a");
    // Both of these ran with their stderr discarded, so "linear AOT audio encode failed" was the
    // whole account of the failure. The files sit inside the scratch directory that is removed on
    // success, and are read back into the error on the paths that keep them.
    let audio_errors = scratch.join("audio.stderr.log");
    let mux_errors = scratch.join("mux.stderr.log");
    let spawn_audio = || {
        Command::new(resolve_runtime_binary("ffmpeg"))
            .args(["-v", "error", "-i", &args.input, "-map", &format!("0:{audio_index}"),
                "-vn", "-c:a", "aac", "-b:a", "192k", "-y"])
            .arg(&audio)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&audio_errors).map(Stdio::from).unwrap_or_else(|_| Stdio::null()))
            .spawn()
            .map_err(|e| format!("spawn linear AOT audio encoder: {e}"))
    };
    let mut audio_child = spawn_audio()?;
    let input_path = Path::new(&args.input);
    log.line(&format!(
        "adopting linear AOT: waiting for {} to finish, encoding audio from stream {} of {} (exists={}) meanwhile; torrent dir holds {}",
        initial.pid,
        audio_index,
        args.input,
        input_path.exists(),
        input_path
            .parent()
            .map(directory_names)
            .unwrap_or_else(|| "unreadable".to_string()),
    ));
    // The frame total used to come from `ffprobe -count_packets`, a full demux of the input, run on
    // a thread for the whole handoff. That put a third reader on the same file the speculative
    // encoder and the AAC pass were already streaming off the bind mount — the slowest resource in
    // the container — for the length of an encode, and it still did not finish in time to report a
    // total. The container header answers the same question in two metadata reads. The exact count
    // is now only paid for at the end, and only if this disagrees with what the AOT produced.
    let (estimate, estimate_source) = handoff_frame_total(&args.input, parent);
    let mut total = estimate.unwrap_or(0);
    let mut total_is_estimate = total != 0;
    if total_is_estimate {
        log.line(&format!("linear AOT handoff expecting {total} frames, from {estimate_source}"));
    } else {
        log.line("linear AOT handoff has no frame total to report: neither the input nor a recorded count could be read");
    }
    let wait_started = std::time::Instant::now();
    let initial_frames = initial.frames;
    let mut last_emit = None;
    // Sampled on every tick because /proc/<pid> is gone the instant the process is, and the size it
    // had reached is the whole question when the kernel is the one that ended it.
    let mut last_rss: Option<u64> = None;
    let mut last_frames = initial_frames;
    let mut missing_since: Option<std::time::Instant> = None;
    let mut reported_missing = false;
    // The AAC pass is started here and used to be waited on only after the video finished, so a job
    // whose audio failed in its first second still spent the whole encode before anyone looked: one
    // run burned eleven minutes and 34,911 successfully encoded frames before reporting that its
    // input had not been there at all. Watch it as we go.
    let mut audio_done: Option<std::process::ExitStatus> = None;
    let mut audio_retry = false;
    let completed = loop {
        if args.cancelfile.as_deref().is_some_and(|path| Path::new(path).exists()) {
            audio_child.kill().ok();
            audio_child.wait().ok();
            std::fs::remove_dir_all(&scratch).ok();
            return Err("cancelled".to_string());
        }
        let state = match pnx264::linear::LinearAotState::read(&state_path) {
            Ok(state) => {
                missing_since = None;
                state
            }
            Err(e) => {
                // The speculative encoder republishes this file about once a second by renaming a
                // temporary over it. `DB` is a bind mount in production, where that rename is not
                // atomic: the path goes briefly absent, and a poll landing in that window used to
                // end an encode that was running perfectly — at 2.9s, 58s, 177s, 337s into four
                // different jobs, with the publisher happily writing the same file for another
                // quarter of an hour afterwards. An absence only means something if it lasts.
                let waiting = *missing_since.get_or_insert(std::time::Instant::now());
                let planner_alive = initial.process_alive();
                if planner_alive && waiting.elapsed() < MISSING_STATE_GRACE {
                    if !reported_missing {
                        log.line(&format!(
                            "linear AOT state momentarily unreadable at {} frames ({e}); waiting for it to come back",
                            last_frames
                        ));
                        reported_missing = true;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                }
                // Gone for good. Which of the two causes it was — one deleted file, or the job's
                // whole scratch directory pulled out from under the encode — is only readable in
                // this moment, so say what still exists while it can still be seen.
                let job_dir = parent.parent().unwrap_or(parent);
                log.line(&format!(
                    "linear AOT handoff lost its state after {} frames: {e} — absent for {:.1}s, planner {} alive, scratch {} exists={}, job dir {} exists={}, siblings={}",
                    last_frames,
                    waiting.elapsed().as_secs_f64(),
                    if planner_alive { "still" } else { "no longer" },
                    parent.display(),
                    parent.exists(),
                    job_dir.display(),
                    job_dir.exists(),
                    directory_names(job_dir),
                ));
                audio_child.kill().ok();
                audio_child.wait().ok();
                std::fs::remove_dir_all(&scratch).ok();
                if !planner_alive {
                    // There is nothing left to adopt, but the episode is not lost: the ordinary
                    // linear encode below still produces it.
                    std::fs::remove_file(&state_path).ok();
                    std::fs::remove_file(&aot_video).ok();
                    return Ok(false);
                }
                return Err(format!("read linear AOT handoff: {e}"));
            }
        };
        if state.compatibility != initial.compatibility {
            audio_child.kill().ok();
            audio_child.wait().ok();
            std::fs::remove_dir_all(&scratch).ok();
            return Err("linear AOT compatibility changed during handoff".to_string());
        }
        let now = std::time::Instant::now();
        if last_emit.is_none_or(|last: std::time::Instant| now.duration_since(last) >= Duration::from_secs(5)) {
            let fps = (state.frames.saturating_sub(initial_frames) as f64
                / wait_started.elapsed().as_secs_f64().max(0.001)).round() as u64;
            let bitrate = media_bitrate_kbps(state.bytes, state.media_micros);
            let fps_value = fps.to_string();
            let frame_value = state.frames.to_string();
            let total_value = total.to_string();
            let bitrate_value = bitrate.to_string();
            println!("{}", pn_emit!(
                protocol = proto,
                negkey = neg,
                schema = [leaf, [leaf, leaf, leaf, leaf]],
                data = ["0", [fps_value, frame_value, total_value, bitrate_value]]
            ).unwrap());
            last_rss = process_rss_mib(state.pid).or(last_rss);
            log.line(&format!(
                "linear AOT handoff waiting: frames={} total={} fps={} bytes={} aot_rss={} {}",
                state.frames,
                if total == 0 {
                    "counting".to_string()
                } else if total_is_estimate {
                    format!("~{total}")
                } else {
                    total.to_string()
                },
                fps,
                state.bytes,
                last_rss.map(|mib| format!("{mib}MiB")).unwrap_or_else(|| "unknown".to_string()),
                memory_line(),
            ));
            last_emit = Some(now);
        }
        last_frames = state.frames;
        if audio_done.is_none() {
            match audio_child.try_wait() {
                Ok(Some(status)) if !status.success() => {
                    // Almost always the input this is reading: on the production bind mount, a file
                    // the speculative encoder holds open is listed in its directory but cannot be
                    // stat'd by name after the download worker renames it, so the audio pass starts
                    // against a path that resolves to nothing while the video sails on. The name
                    // becomes usable again once that process exits, which is exactly what the loop
                    // below is already waiting for — so this is a reason to retry the audio later,
                    // not to throw the encode away.
                    log.line(&format!(
                        "linear AOT audio encode failed after {:.1}s ({}; ffmpeg said: {}); retrying it once the AOT finishes",
                        wait_started.elapsed().as_secs_f64(),
                        exit_reason(&status),
                        tail_line(&audio_errors, 3)
                    ));
                    audio_retry = true;
                    audio_done = Some(status);
                }
                Ok(Some(status)) => audio_done = Some(status),
                Ok(None) => {}
                Err(e) => log.line(&format!("linear AOT audio encode status unreadable: {e}")),
            }
        }
        if state.complete {
            break state;
        }
        if !state.process_alive() {
            // The same silent death as above, except this process watched it happen: report the
            // frames it had reached and the last size it was seen at before falling back.
            log.line(&format!(
                "linear AOT process {} vanished after {} frames ({} of them since this handoff began), last seen at {}; falling back to the linear encode — {}",
                state.pid,
                state.frames,
                state.frames.saturating_sub(initial_frames),
                last_rss.map(|mib| format!("{mib}MiB RSS")).unwrap_or_else(|| "an unknown size".to_string()),
                memory_line(),
            ));
            audio_child.kill().ok();
            audio_child.wait().ok();
            std::fs::remove_dir_all(&scratch).ok();
            std::fs::remove_file(&state_path).ok();
            std::fs::remove_file(&aot_video).ok();
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    // Either the header told us nothing, or what the AOT produced is not what it implied — the one
    // case the exact count exists to catch. Nothing else is reading the input now, so asking costs
    // a read rather than a fight for the disk, and in the ordinary case it is never asked at all.
    if total == 0 || completed.frames.abs_diff(total) > TOTAL_ESTIMATE_TOLERANCE {
        let counted = log.step("ffprobe -count_packets to verify the AOT frame count", || {
            ffprobe_frame(&args.input)
        });
        if let Some(counted) = counted.filter(|counted| *counted != 0) {
            total = counted;
            total_is_estimate = false;
        }
    }
    log.line(&format!(
        "linear AOT reported complete: {} frames, {} bytes, {} expected ({}), {:.1}s waiting",
        completed.frames,
        completed.bytes,
        total,
        if total_is_estimate { estimate_source } else { "counted" },
        wait_started.elapsed().as_secs_f64()
    ));
    // Only the exact count may reject a finished AOT: an estimate is a frame or two out by nature
    // and would throw away a perfectly good encode.
    if !total_is_estimate && total != 0 && completed.frames != total {
        audio_child.kill().ok();
        audio_child.wait().ok();
        return Err(format!(
            "linear AOT encoded {} of the {} frames ffprobe counted in {}",
            completed.frames, total, args.input
        ));
    }
    if audio_retry {
        // The AOT is complete, so whatever was holding the input open has let go of it.
        for attempt in 1..=AUDIO_RETRY_ATTEMPTS {
            if !input_path.exists() {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            log.line(&format!("retrying the linear AOT audio encode (attempt {attempt})"));
            audio_child = spawn_audio()?;
            audio_done = Some(audio_child.wait().map_err(|e| e.to_string())?);
            break;
        }
        if !input_path.exists() {
            return Err(format!(
                "linear AOT audio encode has no input: {} still does not resolve; its directory holds {}",
                args.input,
                input_path
                    .parent()
                    .map(directory_names)
                    .unwrap_or_else(|| "unreadable".to_string()),
            ));
        }
    }
    let audio_status = match audio_done {
        Some(status) => status,
        None => audio_child.wait().map_err(|e| e.to_string())?,
    };
    if !audio_status.success() {
        return Err(format!(
            "linear AOT audio encode failed: {}; ffmpeg said: {}",
            exit_reason(&audio_status),
            tail_line(&audio_errors, 3)
        ));
    }
    // A job whose server publishes HLS and nothing else has no use for the MP4 this would
    // otherwise write: the broker would only take it apart again into the same chunks. Mux the
    // finished video and its AAC track straight into the layout the broker publishes instead.
    let destination = match args.hls.as_deref() {
        Some(directory) => {
            mux_linear_aot_hls(&aot_video, &audio, Path::new(directory), &mux_errors, log)?
        }
        None => {
            let mux_status = Command::new(resolve_runtime_binary("ffmpeg"))
                .args(["-v", "error", "-i"])
                .arg(&aot_video)
                .args(["-i"])
                .arg(&audio)
                .args(["-map", "0:v:0", "-map", "1:a:0", "-c", "copy", "-movflags", "+faststart", "-y"])
                .arg(&output)
                .stderr(std::fs::File::create(&mux_errors).map(Stdio::from).unwrap_or_else(|_| Stdio::null()))
                .status()
                .map_err(|e| format!("spawn linear AOT final mux: {e}"))?;
            if !mux_status.success() {
                return Err(format!(
                    "linear AOT final mux failed: {}; ffmpeg said: {}",
                    exit_reason(&mux_status),
                    tail_line(&mux_errors, 3)
                ));
            }
            output.display().to_string()
        }
    };
    log.line(&format!(
        "linear AOT handoff complete: {} frames, {} bytes muxed to {}",
        completed.frames,
        completed.bytes,
        destination
    ));
    std::fs::remove_file(&aot_video).ok();
    std::fs::remove_file(&state_path).ok();
    std::fs::remove_dir_all(&scratch).ok();
    Ok(true)
}

fn emit_extract_failure(proto: &Protocol, neg: &str) {
    println!(
        "{}",
        pn_emit!(
            protocol = proto,
            negkey = neg,
            schema = [leaf, leaf],
            data = ["2", "ERROR"]
        )
        .unwrap()
    );
}

// The canonical name behind whichever way a preset was asked for. The boolean flags predate
// `--preset` and stay: they are how the encode worker has always spelled its choice, and how a
// standalone run is written by hand.
fn selected_preset(args: &Args) -> Option<String> {
    if let Some(name) = args.preset.as_deref() {
        let name = name.trim();
        if !name.is_empty() {
            return Some(name.to_ascii_lowercase());
        }
    }
    let name = if args.gpu {
        "gpu"
    } else if args.x264 {
        "standard"
    } else if args.pseudolossless {
        "pseudolossless"
    } else if args.veryslow {
        "veryslow"
    } else if args.p720 {
        "720p"
    } else if args.p480 {
        "480p"
    } else if args.dummy {
        "dummy"
    } else {
        return None;
    };
    Some(name.to_string())
}

// The preset this run encodes with, or None for a run that encodes nothing of its own — a concat
// or a legacy concat, which stitch an intro onto a file that has already been encoded.
//
// One function, because every decision downstream has to be made about the same preset: the ffmpeg
// parameters, the settings the ahead-of-time encoder drives libx264 with, whether it may encode
// ahead at all, and the height the output is named after. When those were four separate readings
// of the same boolean flags, `--preset standard` and `--x264` selected the same parameters and
// disagreed about everything else.
//
// An unknown name is fatal rather than silently standard: the caller asked for particular settings,
// and encoding at different ones is a release that has to be made again.
fn active_preset(args: &Args) -> Option<ResolvedPreset> {
    let name = if args.gpu
        || args.x264
        || args.pseudolossless
        || args.veryslow
        || args.p720
        || args.p480
    {
        selected_preset(args)
    } else if args.concat || args.legacyconcat {
        // A preset flag still wins over concat, exactly as it did when this was a chain of
        // `else if`s; without one, a concat is not an encode and has no preset.
        None
    } else {
        Some(selected_preset(args).unwrap_or_else(|| "standard".to_string()))
    }?;
    match resolve_preset(&name) {
        Some(resolved) => Some(resolved),
        None => panic!("Unknown preset `{}`.", name),
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    // Opened before anything else runs: everything below this line used to be invisible, because
    // the --logfile transcript is only created once ffmpeg itself starts.
    let mut log = ToolLog::beside(args.logfile.as_deref());
    log.line(&format!(
        "pnmpeg start input={} output={} ass={:?} lang={:?} intro_dir={:?} candidates={}",
        args.input, args.output, args.ass, args.lang, args.intro_dir, args.candidate.len()
    ));
    log.line(&format!(
        "mode preset={:?} concat={} legacyconcat={} joinconcat={} joinass={} studio={} extractsubs={}",
        selected_preset(&args),
        args.concat, args.legacyconcat, args.joinconcat, args.joinass, args.studio, args.extractsubs
    ));
    // Resolved once, here, because four separate decisions depend on it: the ffmpeg parameters,
    // the AOT encoder's settings, whether AOT may run at all, and the height the output is named
    // after. Asking the flags for any of those means a preset reached by `--preset` answers
    // differently from the same preset reached by its flag.
    let selected = if args.gpu { 1 } else { 0 }
        + if args.x264 { 1 } else { 0 }
        + if args.pseudolossless { 1 } else { 0 }
        + if args.veryslow { 1 } else { 0 }
        + if args.p720 { 1 } else { 0 }
        + if args.p480 { 1 } else { 0 }
        + if args.dummy { 1 } else { 0 }
        + if args.preset.is_some() { 1 } else { 0 };
    if selected > 1 {
        panic!("You must use one preset at a time.");
    }
    let active_preset = active_preset(&args);
    if let Some(preset) = &active_preset {
        log.line(&format!(
            "preset {} ({}, {})",
            preset.name,
            preset.hardware.label(),
            if preset.from_file { "from file" } else { "built in" },
        ));
    }
    let x264_config = planner_encoder_config(active_preset.as_ref());
    if args.linear_prefix {
        let Some(job_id) = args.aot_job_id else {
            eprintln!("pnmpeg: --aot-job-id is required with --linear-prefix");
            std::process::exit(2);
        };
        let Some(ass) = args.ass.as_deref() else {
            eprintln!("pnmpeg: --ass is required with --linear-prefix");
            std::process::exit(2);
        };
        let Some(preset) = active_preset.as_ref() else {
            eprintln!("pnmpeg: --linear-prefix needs a preset to encode with");
            std::process::exit(2);
        };
        let output = PathBuf::from(&args.output);
        let compatibility = preset.aot_compatibility();
        let state_path = output
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("linear-aot.state");
        log.line(&format!(
            "linear AOT planner start: job={} pid={} output={} {}",
            job_id,
            std::process::id(),
            output.display(),
            memory_line()
        ));
        // The planner spends its whole life inside one blocking call, and when the kernel kills it
        // there it writes nothing. A watchdog on a second handle to the same transcript leaves a
        // trail of frame counts and resident sizes, so a log that simply stops still says how big
        // this process had grown and what the host had left at that moment.
        let watchdog_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watchdog = {
            let stop = watchdog_stop.clone();
            let mut watchdog_log = log.watchdog();
            let state_path = state_path.clone();
            std::thread::spawn(move || {
                let pid = std::process::id();
                let mut next = std::time::Instant::now() + Duration::from_secs(15);
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(250));
                    if std::time::Instant::now() < next {
                        continue;
                    }
                    next = std::time::Instant::now() + Duration::from_secs(15);
                    let progress = pnx264::linear::LinearAotState::read(&state_path)
                        .map(|state| format!("frames={} bytes={}", state.frames, state.bytes))
                        .unwrap_or_else(|e| format!("state unreadable ({e})"));
                    watchdog_log.line(&format!(
                        "linear AOT planner alive: {} rss={} {}",
                        progress,
                        process_rss_mib(pid)
                            .map(|mib| format!("{mib}MiB"))
                            .unwrap_or_else(|| "unknown".to_string()),
                        memory_line(),
                    ));
                }
            })
        };
        let result = pnx264::linear::run_linear_aot(pnx264::linear::LinearAotConfig {
            ffmpeg: resolve_runtime_binary("ffmpeg"),
            prefix_state: PathBuf::from(&args.input),
            state: state_path,
            output,
            subtitle: PathBuf::from(ass),
            cancel_file: args.cancelfile.as_deref().map(PathBuf::from),
            busy_file: args.aot_busyfile.as_deref().map(PathBuf::from),
            lease_file: args.aot_lockfile.as_deref().map(PathBuf::from),
            job_id,
            compatibility,
            filter: preset
                .video_filter()
                .unwrap_or_else(|| format!("ass={},format=yuv420p", pnx264::linear::SUBTITLE_TOKEN)),
            video_args: preset.video_encoder_args(),
        });
        watchdog_stop.store(true, std::sync::atomic::Ordering::Relaxed);
        watchdog.join().ok();
        match result {
            Ok(summary) => {
                log.line(&format!(
                    "linear AOT complete: {} frames, {} bytes, {}",
                    summary.frames,
                    summary.bytes,
                    memory_line()
                ));
                return;
            }
            Err(error) => {
                log.line(&format!("linear AOT failed: {error} — {}", memory_line()));
                eprintln!("pnmpeg linear AOT failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if args.aot_chunk {
        let required = |value: Option<u64>, name: &str| {
            value.unwrap_or_else(|| {
                eprintln!("pnmpeg: {name} is required with --aot-chunk");
                std::process::exit(2);
            })
        };
        let required_u32 = |value: Option<u32>, name: &str| required(value.map(u64::from), name) as u32;
        let Some(ass) = args.ass.as_deref() else {
            eprintln!("pnmpeg: --ass is required with --aot-chunk");
            std::process::exit(2);
        };
        let Some(stop_file) = args.aot_stopfile.as_deref() else {
            eprintln!("pnmpeg: --aot-stopfile is required with --aot-chunk");
            std::process::exit(2);
        };
        let encoder = pnx264::Config {
            crf: args.aot_crf.unwrap_or(18.0),
            threads: 1,
            preset: args.aot_preset.clone(),
            tune: args.aot_tune.clone(),
            profile: args.aot_profile.clone(),
            level: args.aot_level.clone(),
            x264_params: args.aot_x264_params.clone(),
            plan_only: false,
            ..Default::default()
        };
        let result = pnx264::parallel::encode_aot_chunk(pnx264::parallel::AotChunkConfig {
            ffmpeg: resolve_runtime_binary("ffmpeg"),
            input: PathBuf::from(&args.input),
            output: PathBuf::from(&args.output),
            subtitle: PathBuf::from(ass),
            cancel_file: args.cancelfile.as_deref().map(PathBuf::from),
            stop_file: PathBuf::from(stop_file),
            busy_file: args.aot_busyfile.as_deref().map(PathBuf::from),
            width: required_u32(args.video_width, "--video-width"),
            height: required_u32(args.video_height, "--video-height"),
            fps_num: required_u32(args.fps_num, "--fps-num"),
            fps_den: required_u32(args.fps_den, "--fps-den"),
            start: required(args.start_frame, "--start-frame"),
            frames: required(args.frame_count, "--frame-count"),
            encoder,
        });
        if let Err(error) = result {
            log.line(&format!("AOT chunk failed: {error} — {}", memory_line()));
            eprintln!("pnmpeg AOT chunk failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    if args.plan_prefix {
        let Some(ass) = args.ass.as_deref() else {
            eprintln!("pnmpeg: --ass is required with --plan-prefix");
            std::process::exit(2);
        };
        let result = pnx264::planner::run_prefix_planner(pnx264::planner::PrefixPlannerConfig {
            ffmpeg: resolve_runtime_binary("ffmpeg"),
            executable: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("pnmpeg")),
            prefix_state: PathBuf::from(&args.input),
            output: PathBuf::from(&args.output),
            subtitle: PathBuf::from(ass),
            cancel_file: args.cancelfile.as_deref().map(PathBuf::from),
            busy_file: args.aot_busyfile.as_deref().map(PathBuf::from),
            lease_file: args.aot_lockfile.as_deref().map(PathBuf::from),
            workers: std::env::var("PN_PARALLEL_WORKERS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| std::thread::available_parallelism().map(|value| value.get()).unwrap_or(1)),
            encoder: x264_config.clone(),
        });
        match result {
            Ok(summary) => {
                log.line(&format!(
                    "prefix plan complete: {}/{} frames, {} IDRs, {} bytes, {} AOT chunks launched",
                    summary.planned, summary.submitted, summary.idrs, summary.source_bytes,
                    summary.aot_chunks,
                ));
                return;
            }
            Err(e) => {
                log.line(&format!("prefix plan failed: {e} — {}", memory_line()));
                eprintln!("pnmpeg prefix planner failed: {e}");
                std::process::exit(1);
            }
        }
    }

    let mut proto = Protocol::new(vec![1]);
    let neg = proto.request(ToolInfo { tool: match args.negotiator {
                        Some(ref negotiator) => negotiator,
                        None => "PNmpeg",
                    }, build: match args.negver {
                        Some(ref negver) => negver,
                        None => "0.1.1",
                    }, proto: 1 },
                  ToolInfo { tool: "PNmpeg", build: "0.1.1", proto: 1 },
                  match args.negkey.as_ref() {
                      Some(key) => key.clone(),
                      None => "PNmpegCLI".to_string(),
                  });

    let encoder = FFmpeg::new();

    // Extraction reads the container and writes sidecar files; it shares nothing
    // with the encode pipeline below, so it answers and exits on its own.
    if args.extractsubs {
        let input = PathBuf::from(&args.input);
        let output_dir = PathBuf::from(&args.output);
        if let Err(e) = tokio::fs::create_dir_all(&output_dir).await {
            eprintln!("[pnmpeg] subtitle output directory failed: {e}");
            emit_extract_failure(&proto, &neg);
            std::process::exit(1);
        }
        let streams = log.step("ffprobe subtitle streams", || ffprobe_subtitle_streams(&input));
        log.line(&format!("{} subtitle stream(s) found", streams.len()));
        for stream in &streams {
            let outcome = extract_subtitle(&input, &output_dir, stream);
            let (path, detail) = match &outcome {
                ExtractOutcome::Extracted(extracted) => (
                    extracted
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    String::new(),
                ),
                ExtractOutcome::Skipped { reason, .. } => (String::new(), reason.clone()),
            };
            let ordinal = stream.ordinal.to_string();
            let language = stream.language.clone().unwrap_or_default();
            let title = stream.title.clone().unwrap_or_default();
            let codec = stream.codec.clone();
            println!(
                "{}",
                pn_emit!(
                    protocol = proto,
                    negkey = &neg,
                    schema = [leaf, [leaf, leaf, leaf, leaf, leaf, leaf]],
                    data = ["4", [ordinal, language, title, codec, path, detail]]
                )
                .unwrap()
            );
        }
        println!(
            "{}",
            pn_emit!(
                protocol = proto,
                negkey = &neg,
                schema = [leaf, leaf],
                data = ["1", "DONE"]
            )
            .unwrap()
        );
        return;
    }

    if args.studio {
        let manifest_bytes = match tokio::fs::read(&args.input).await {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("Studio manifest read failed: {}", e);
                std::process::exit(1);
            }
        };
        let manifest: StudioRenderManifest = match serde_json::from_slice(&manifest_bytes) {
            Ok(manifest) => manifest,
            Err(e) => {
                eprintln!("Studio manifest is invalid: {}", e);
                std::process::exit(1);
            }
        };
        if manifest.sources.is_empty() || manifest.total_duration_ms == 0 {
            eprintln!("Studio manifest has no usable video sources");
            std::process::exit(1);
        }
        let manifest_path = PathBuf::from_str(&args.input).unwrap_or_else(|_| PathBuf::from("studio.json"));
        let concat_path = manifest_path.parent().unwrap_or(std::path::Path::new("."))
            .join("studio.ffconcat");
        if let Err(e) = write_ffconcat(&concat_path, &manifest.sources) {
            eprintln!("Studio concat list failed: {}", e);
            std::process::exit(1);
        }
        let params = studio_ffmpeg_params(&manifest, &concat_path, std::path::Path::new(&args.output));
        let totalframe = (manifest.render_duration_ms() as f64
            * manifest.fps_num as f64 / manifest.fps_den.max(1) as f64 / 1000.0)
            .ceil() as u64;
        log.line(&format!(
            "studio manifest: {} source(s), {}ms, {} frames",
            manifest.sources.len(), manifest.total_duration_ms, totalframe
        ));
        run_with_progress(&mut proto, &neg, encoder, params, totalframe, args.cancelfile, args.logfile, &mut log).await;
        return;
    }

    let concfilepath = PathBuf::from_str(&args.input).unwrap()
        .parent().unwrap()
        .canonicalize().unwrap()
        .join("PNmpeg_Concat.txt");

    let intro_dir = args.intro_dir.as_deref().filter(|path| !path.trim().is_empty());
    let selected_subinput = match intro_dir {
        Some(intro_dir) => match log.step(
            &format!("prepare_compatible_intro from {} (may transcode the intro)", intro_dir),
            || prepare_compatible_intro(Path::new(&args.input), Path::new(intro_dir)),
        ) {
            Ok(path) => Some(path.display().to_string()),
            Err(e) => {
                log.line(&format!("intro preparation failed: {}", e));
                eprintln!("Intro preparation failed: {}", e);
                std::process::exit(1);
            }
        },
        None => log.step("select_subinput", || select_subinput(&args.input, &args.candidate, &args.subinput)),
    };
    log.line(&format!("selected_subinput={:?}", selected_subinput));

    if args.joinconcat || args.joinass {
        let mut join_inputs = Vec::new();
        if intro_dir.is_some() || args.subinput.is_some() {
            if let Some(intro) = &selected_subinput {
                join_inputs.push(intro.clone());
            }
        }
        join_inputs.push(args.input.clone());
        join_inputs.extend(args.candidate.iter().cloned());
        let mut totalframe: u64 = 0;
        let parent = PathBuf::from_str(&args.input).unwrap()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let joinfile = parent
            .canonicalize()
            .unwrap_or(parent)
            .join("PNmpeg_Keycode_Concat.txt");
        let mut file = File::create(&joinfile).await.unwrap();
        log.line(&format!("join inputs: {:?}", join_inputs));
        for input in &join_inputs {
            let counted = log.step(
                &format!("ffprobe -count_packets {} (full demux)", input),
                || ffprobe_frame(input),
            );
            log.line(&format!("{} -> {:?} frames", input, counted));
            totalframe += counted.unwrap_or(0);
            let canon = PathBuf::from_str(input).unwrap()
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(input))
                .display()
                .to_string();
            file.write(format!("file '{}'\n", canon.replace('\'', "'\\''")).as_bytes()).await.unwrap();
        }
        drop(file);

        let mut params = if args.joinconcat {
            Vec::from(CONCAT)
        } else {
            // The join re-encodes, so it wants the same standard preset an ordinary encode uses —
            // including an operator's file, or a join would ship at different settings.
            let mut p = resolve_preset("standard").unwrap().params;
            p.insert(0, FfmpegParams::Safe(Cow::Borrowed("0")));
            p.insert(0, FfmpegParams::Format(Cow::Borrowed("concat")));
            p
        };
        for i in params.iter_mut() {
            match i {
                FfmpegParams::Input(a) => {
                    let c = a
                        .replace("CONCATFILEV", &joinfile.display().to_string())
                        .replace("INPUTFILEV", &joinfile.display().to_string());
                    *i = FfmpegParams::Input(Cow::Owned(c));
                }
                FfmpegParams::BasicFilter(a) => {
                    if let Some(ref b) = args.ass {
                        let ass = quote_filter_value(b);
                        *i = FfmpegParams::BasicFilter(Cow::Owned(a.replace("INPUTFILEASS", &ass)));
                    }
                }
                FfmpegParams::Map(a) => {
                    *i = FfmpegParams::Map(Cow::Owned(a.replace("JPN_INDEX", "a:0")));
                }
                FfmpegParams::Output(a) => {
                    *i = FfmpegParams::Output(Cow::Owned(a.replace("OUTFILEV", &args.output)));
                }
                _ => {}
            }
        }
        log.line(&format!("join totalframe={}", totalframe));
        run_with_progress(&mut proto, &neg, encoder, params, totalframe, args.cancelfile, args.logfile, &mut log).await;
        return;
    }

    let subinput_for_legacy = selected_subinput.clone();
    let input_for_legacy = args.input.clone();
    let use_legacy = args.concat && intro_dir.is_none() && !args.candidate.is_empty() && log.step(
        "ffprobe framerate/samplerate compatibility check",
        || subinput_for_legacy.as_ref().map(|p| {
            ffprobe_framerate(p) != ffprobe_framerate(&input_for_legacy) ||
            ffprobe_samplerate(p) != ffprobe_samplerate(&input_for_legacy)
        }).unwrap_or(false),
    );
    log.line(&format!("use_legacy={}", use_legacy));

    let mut concfile = match args.concat && !use_legacy {
        true => Some(File::create(&concfilepath).await.unwrap()),
        false => None,
    };

    // The preset was resolved once at startup; this is the same answer, not a second reading of the
    // flags. The concat tables stay compiled in — they are not a quality choice, they are how an
    // intro is stitched on — and `active_preset` is None for exactly the runs that need them.
    let mut params: Vec<FfmpegParams> = match &active_preset {
        Some(preset) => preset.params.clone(),
        None if args.concat && !use_legacy => Vec::from(CONCAT),
        None => Vec::from(CONCAT_LEGACY),
    };

    log.line(&format!("{} ffmpeg parameter(s) from the selected preset", params.len()));
    let audio_index = if !args.concat || args.legacyconcat {
        let lang = args.lang.clone();
        let input = args.input.clone();
        log.step("ffprobe audio language streams", || {
            lang.as_deref()
                .and_then(|lang| ffprobe_lang(&input, lang).map(|idx| idx.to_string()))
                .unwrap_or_else(|| wrap("a:0"))
        })
    } else {
        wrap("1")
    };
    log.line(&format!("audio_index={}", audio_index));

    if adopts_linear_prefix(active_preset.as_ref()) {
        let preset = active_preset.as_ref().expect("adopts_linear_prefix implies a preset");
        match finish_linear_aot(&args, preset, &audio_index, &proto, &neg, &mut log) {
            Ok(true) => {
                println!("{}", pn_emit!(
                    protocol = proto,
                    negkey = &neg,
                    schema = [leaf, leaf],
                    data = ["1", "DONE"]
                ).unwrap());
                return;
            }
            Ok(false) => {}
            Err(error) if error == "cancelled" => {
                log.line("linear AOT handoff cancelled");
                println!("{}", pn_emit!(
                    protocol = proto,
                    negkey = &neg,
                    schema = [leaf, leaf],
                    data = ["3", "CANCELFILE"]
                ).unwrap());
                return;
            }
            Err(error) => {
                log.line(&format!("linear AOT handoff failed: {error}"));
                eprintln!("pnmpeg linear AOT handoff failed: {error}");
                println!("{}", pn_emit!(
                    protocol = proto,
                    negkey = &neg,
                    schema = [leaf, leaf],
                    data = ["2", "0"]
                ).unwrap());
                return;
            }
        }
    }

    // The corrected episode-scale benchmark showed a latency win only for VerySlow. Every other
    // CPU preset stays on the established linear ffmpeg path below.
    if encodes_in_chunks(active_preset.as_ref()) && !args.concat && !args.legacyconcat {
        let Some(ass) = args.ass.as_deref() else {
            eprintln!("pnmpeg: --ass is required for parallel VerySlow encoding");
            std::process::exit(2);
        };
        let output = PathBuf::from(&args.output);
        let plan = output.parent().map(|parent| parent.join("parallel.plan"));
        let workers = std::env::var("PN_PARALLEL_WORKERS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| std::thread::available_parallelism().map(|value| value.get()).unwrap_or(1));
        // The headroom cap exists because a 12-worker fallback was OOM-killed mid-encode. Recording
        // what it decided, and on what reading, is what makes the next one answerable without
        // reconstructing the host's memory after the fact.
        log.line(&format!(
            "parallel VerySlow requested with {} workers, {} after memory headroom — {}",
            workers,
            pnx264::parallel::memory_worker_limit(workers),
            memory_line()
        ));
        let mut encoder = x264_config.clone();
        encoder.plan_only = false;
        let result = pnx264::parallel::encode_parallel(
            pnx264::parallel::ParallelConfig {
                ffmpeg: resolve_runtime_binary("ffmpeg"),
                ffprobe: resolve_runtime_binary("ffprobe"),
                input: PathBuf::from(&args.input),
                output,
                subtitle: PathBuf::from(ass),
                cancel_file: args.cancelfile.as_deref().map(PathBuf::from),
                plan: plan.filter(|path| path.exists()),
                workers,
                encoder,
                audio_map: audio_index.clone(),
            },
            |update| {
                let fps = update.fps.round() as u64;
                let bitrate = media_bitrate_kbps(update.bytes, update.media_micros);
                let frame = update.frames;
                let total = update.total_frames;
                println!("{}", pn_emit!(
                    protocol = proto,
                    negkey = &neg,
                    schema = [leaf, [leaf, leaf, leaf, leaf]],
                    data = ["0", [fps, frame, total, bitrate]]
                ).unwrap());
            },
        );
        match result {
            Ok(summary) => {
                log.line(&format!(
                    "parallel VerySlow done: {} frames (total from {}), {} chunks, {} workers, {} AOT chunks reused, natural={}, {:.2}s",
                    summary.frames,
                    summary.frame_total.label(),
                    summary.chunks,
                    summary.workers,
                    summary.reused_chunks,
                    summary.natural_boundaries,
                    summary.elapsed.as_secs_f64(),
                ));
                println!("{}", pn_emit!(
                    protocol = proto,
                    negkey = &neg,
                    schema = [leaf, leaf],
                    data = ["1", "DONE"]
                ).unwrap());
                return;
            }
            Err(e) if e == "parallel input has fewer than four chunks per worker" => {
                log.line(&format!("parallel VerySlow skipped: {e}; using linear ffmpeg"));
            }
            Err(e) if e == "cancelled" || args.cancelfile.as_deref().is_some_and(|path| Path::new(path).exists()) => {
                log.line("parallel VerySlow cancelled");
                println!("{}", pn_emit!(
                    protocol = proto,
                    negkey = &neg,
                    schema = [leaf, leaf],
                    data = ["3", "CANCELFILE"]
                ).unwrap());
                return;
            }
            Err(e) => {
                log.line(&format!("parallel VerySlow failed: {e} — {}", memory_line()));
                eprintln!("pnmpeg parallel encode failed: {e}");
                println!("{}", pn_emit!(
                    protocol = proto,
                    negkey = &neg,
                    schema = [leaf, leaf],
                    data = ["2", "0"]
                ).unwrap());
                return;
            }
        }
    }

    let mut totalframe: u64 = 0;
    for i in params.iter_mut() {
        match i {
            FfmpegParams::Map(a) => {
                *i = FfmpegParams::Map(Cow::Owned(a.replace("JPN_INDEX", &format!("{}", audio_index))));
            },
            FfmpegParams::Input(a) => {
                let mut c = a.to_string();
                // What gets counted is not always what ffmpeg is handed here. The concat branch
                // passes a list file, and `-f concat` is what makes that readable — ffprobe on its
                // own sees an unrecognised text file and fails, so counting the argument scored the
                // whole intro pass as zero frames and its progress line rendered `frame / 0` with
                // no percentage and no ETA. Count the files that go into the list instead: their
                // sum is what the concatenated output holds.
                let mut counted_inputs: Vec<String> = Vec::new();
                if let Some(ref mut file) = concfile {
                    if let Some(ref b) = selected_subinput {
                        let canon_input = PathBuf::from_str(&args.input).unwrap().canonicalize().unwrap().display().to_string();
                        let canon_snput = PathBuf::from_str(b).unwrap().canonicalize().unwrap().display().to_string();
                        file.write(format!("file '{}'\nfile '{}'\n", canon_snput, canon_input).as_bytes()).await.unwrap();
                        counted_inputs.push(canon_snput);
                        counted_inputs.push(canon_input);
                    }
                    c = c.replace("CONCATFILEV", &concfilepath.display().to_string());
                } else {
                    c = c.replace("INPUTFILEV", &args.input);
                    if let Some(ref b) = selected_subinput {
                        c = c.replace("CONCATFILEV", b);
                    }
                    counted_inputs.push(c.clone());
                }
                // The long pole before ffmpeg starts: -count_packets demuxes the whole file, and
                // until this log existed a slow or hung count looked exactly like a dead encoder.
                for input in &counted_inputs {
                    let counted = log.step(
                        &format!("ffprobe -count_packets {} (full demux)", input),
                        || ffprobe_frame(input),
                    );
                    log.line(&format!("{} -> {:?} frames", input, counted));
                    totalframe += counted.unwrap_or(0);
                }
                *i = FfmpegParams::Input(Cow::Owned(c));
            },
            FfmpegParams::BasicFilter(a) => {
                if let Some(ref b) = args.ass {
                    let ass = quote_filter_value(b);
                    *i = FfmpegParams::BasicFilter(Cow::Owned(a.replace("INPUTFILEASS", &ass)));
                }
            }
            FfmpegParams::Output(a) => {
                *i = FfmpegParams::Output(Cow::Owned(a.replace("OUTFILEV", &args.output)));
            }
            FfmpegParams::R(a) => {
                if a.contains("FPSV") {
                    let fps = log.step("ffprobe framerate", || ffprobe_framerate(&args.input))
                        .map(|(n, d)| format!("{}/{}", n, d))
                        .unwrap_or_else(|| "24".to_string());
                    *i = FfmpegParams::R(Cow::Owned(a.replace("FPSV", &fps)));
                }
            },
            _ => ()
        }
    }

    // A server that publishes HLS and nothing else has no use for the MP4 this run would otherwise
    // write — the broker would only take it apart into the same chunks. The intro concat arrives
    // here too, and is as much a final mux as the encode: it stream-copies what it joins.
    if let Some(directory) = args.hls.as_deref() {
        if let Err(error) = retarget_params_to_hls(&mut params, Path::new(directory), &args.input, scale_height(active_preset.as_ref()), &mut log) {
            log.line(&format!("HLS output could not be prepared: {error}"));
            eprintln!("pnmpeg HLS output could not be prepared: {error}");
            println!("{}", pn_emit!(
                protocol = proto,
                negkey = &neg,
                schema = [leaf, leaf],
                data = ["2", "0"]
            ).unwrap());
            return;
        }
    }

    log.line(&format!("totalframe={} — handing off to ffmpeg", totalframe));
    run_with_progress(&mut proto, &neg, encoder, params, totalframe, args.cancelfile, args.logfile, &mut log).await;
}

async fn run_with_progress(
    proto: &mut Protocol,
    neg: &str,
    mut encoder: FFmpeg,
    params: Vec<FfmpegParams>,
    totalframe: u64,
    cancelfile: Option<String>,
    logfile: Option<String>,
    log: &mut ToolLog,
) {
    log.line(&format!("spawning ffmpeg ({} expected frames)", totalframe));
    // A total of zero is not fatal — the encode runs and reports frames either way — but every
    // percentage and ETA downstream divides by it, so the whole run renders as `frame / 0` with no
    // progress bar. That is worth naming here, because it means a `-count_packets` above failed and
    // said so only by returning None.
    if totalframe == 0 {
        log.line("WARNING: counted 0 total frames — progress will carry no percentage or ETA");
    }
    let (tx, mut rx): (UnboundedSender<RpbData>, UnboundedReceiver<RpbData>) = mpsc::unbounded_channel();
    let _thr = tokio::spawn(async move {
        do_comm_encode_ffmpeg(
            &mut encoder,
            params,
            tx,
            Some(totalframe),
            cancelfile,
            logfile,
        ).await;
    });

    let mut last: Option<Instant> = None;
    let mut first_progress = true;
    while let Some(val) = rx.recv().await {
        match val {
            RpbData::Progress(fps, frame, total, bitrate) => {
                if first_progress {
                    // The single most useful line in the file: everything before it is setup, and
                    // a run that never reaches it never started encoding at all.
                    log.line(&format!("first ffmpeg progress frame={} fps={}", frame, fps));
                    first_progress = false;
                }
                if last.map(|t| t.elapsed() < Duration::from_secs(5)).unwrap_or(false) {
                    continue;
                }
                last = Some(Instant::now());
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = neg,
                        schema = [leaf, [leaf, leaf, leaf, leaf]],
                        data   = ["0", [fps, frame, total, bitrate]]
                    ).unwrap()
                )
            }
            RpbData::Warning(warning) => {
                log.line(&format!("warning: {}", warning));
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = neg,
                        schema = [leaf, leaf],
                        data   = ["4", warning]
                    ).unwrap()
                )
            }
            RpbData::Done(a) => {
                log.line(&format!("ffmpeg done: {}", a));
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = neg,
                        schema = [leaf, leaf],
                        data   = ["1", a]
                    ).unwrap()
                )
            }
            RpbData::Fail => {
                log.line("ffmpeg failed");
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = neg,
                        schema = [leaf, leaf],
                        data   = ["2", "0"]
                    ).unwrap()
                )
            }
            RpbData::CancelFile => {
                log.line("ffmpeg cancelled");
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = neg,
                        schema = [leaf, leaf],
                        data   = ["3", "CANCELFILE"]
                    ).unwrap()
                )
            }
        }
    }
}

fn prepare_compatible_intro(main: &Path, intro_dir: &Path) -> Result<PathBuf, String> {
    let target = ffprobe_concat_media(main)
        .ok_or_else(|| format!("could not probe concat streams in `{}`", main.display()))?;
    if target.video_codec != "h264" || target.audio_codec != "aac" {
        return Err(format!(
            "unsupported concat target codecs {}/{} (expected h264/aac)",
            target.video_codec, target.audio_codec
        ));
    }
    let mut files = std::fs::read_dir(intro_dir)
        .map_err(|e| format!("could not read `{}`: {}", intro_dir.display(), e))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let temporary = path.file_name().and_then(|name| name.to_str())
                .map(|name| name.contains(".tmp."))
                .unwrap_or(false);
            path.is_file() && !temporary && path.extension().and_then(|ext| ext.to_str()).map(|ext| {
                matches!(ext.to_ascii_lowercase().as_str(), "mp4" | "mkv" | "mov" | "webm" | "m4v")
            }).unwrap_or(false)
        })
        .collect::<Vec<_>>();
    files.sort();
    if files.is_empty() {
        return Err(format!("intro folder `{}` contains no videos", intro_dir.display()));
    }

    let mut source: Option<(PathBuf, ConcatMedia)> = None;
    for path in &files {
        let Some(media) = ffprobe_concat_media(path) else {
            continue;
        };
        if media == target {
            return Ok(path.clone());
        }
        let generated = path.file_name().and_then(|name| name.to_str())
            .map(|name| name.starts_with("pnmpeg_compat_"))
            .unwrap_or(false);
        let replace = match &source {
            None => true,
            Some((current_path, current)) => {
                let current_generated = current_path.file_name().and_then(|name| name.to_str())
                    .map(|name| name.starts_with("pnmpeg_compat_"))
                    .unwrap_or(false);
                (current_generated && !generated)
                    || (current_generated == generated
                        && (media.fps_num as u64 * current.fps_den as u64
                            > current.fps_num as u64 * media.fps_den as u64))
            }
        };
        if replace {
            source = Some((path.clone(), media));
        }
    }
    let (source, _) = source.ok_or_else(|| {
        format!("intro folder `{}` contains no probeable H.264/AAC video", intro_dir.display())
    })?;
    let signature = serde_json::to_vec(&target).map_err(|e| e.to_string())?;
    let signature_hash = format!("{:x}", md5::compute(signature));
    let cache = intro_dir.join(format!("pnmpeg_compat_{}.mp4", signature_hash));
    let temporary = intro_dir.join(format!("pnmpeg_compat_{}.tmp.mp4", signature_hash));
    std::fs::remove_file(&temporary).ok();
    encode_compatible_intro(&source, &temporary, &target)?;
    let encoded = ffprobe_concat_media(&temporary)
        .ok_or_else(|| "could not probe generated intro variant".to_string())?;
    if encoded != target {
        std::fs::remove_file(&temporary).ok();
        return Err(format!("generated intro is still incompatible: {:?} != {:?}", encoded, target));
    }
    if cache.exists() {
        std::fs::remove_file(&cache).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&temporary, &cache).map_err(|e| e.to_string())?;
    Ok(cache)
}

fn encode_compatible_intro(source: &Path, output: &Path, target: &ConcatMedia) -> Result<(), String> {
    use pandora_toolchain::lib::mpeg::core::run_ffmpeg_params;

    let sar = target.sample_aspect_ratio.replace(':', "/");
    let filter = format!(
        "scale={}:{}:flags=lanczos,setsar={},format={}",
        target.width, target.height, sar, target.pixel_format
    );
    let fps = format!("{}/{}", target.fps_num, target.fps_den);
    let ok = run_ffmpeg_params(vec![
        FfmpegParams::Overwrite,
        FfmpegParams::Input(Cow::Owned(source.display().to_string())),
        FfmpegParams::Map(Cow::Borrowed("0:v:0")),
        FfmpegParams::Map(Cow::Borrowed("0:a:0")),
        FfmpegParams::BasicFilter(Cow::Owned(filter)),
        FfmpegParams::Cv(Cow::Borrowed("libx264")),
        FfmpegParams::Profile(Cow::Borrowed("high")),
        FfmpegParams::Level(Cow::Borrowed("4.1")),
        FfmpegParams::Crf(17),
        FfmpegParams::Preset(Cow::Borrowed("fast")),
        FfmpegParams::R(Cow::Owned(fps)),
        FfmpegParams::Ca(Cow::Borrowed("aac")),
        FfmpegParams::Ba(Cow::Borrowed("192k")),
        FfmpegParams::Ar(Cow::Owned(target.sample_rate.to_string())),
        FfmpegParams::Ac(Cow::Owned(target.channels.to_string())),
        FfmpegParams::Movflags,
        FfmpegParams::Output(Cow::Owned(output.display().to_string())),
    ]);
    if ok {
        Ok(())
    } else {
        std::fs::remove_file(output).ok();
        Err(format!("ffmpeg could not convert intro `{}`", source.display()))
    }
}

fn select_subinput(input: &String, candidates: &Vec<String>, subinput: &Option<String>) -> Option<String> {
    if !candidates.is_empty() {
        let main_fps = ffprobe_framerate(input);
        let main_sr = ffprobe_samplerate(input);
        let mut best_match: Option<(usize, &String)> = None;
        let mut highest_fps: Option<(&String, (u32, u32))> = None;
        for candidate in candidates {
            let cand_fps = ffprobe_framerate(candidate);
            let cand_sr = ffprobe_samplerate(candidate);
            if let Some(fps) = cand_fps {
                match highest_fps {
                    None => highest_fps = Some((candidate, fps)),
                    Some((_, hfps)) => {
                        if fps.0 * hfps.1 > hfps.0 * fps.1 {
                            highest_fps = Some((candidate, fps));
                        }
                    }
                }
            }

            let mut score = 0usize;
            if main_fps.is_some() && cand_fps == main_fps { score += 1; }
            if main_sr.is_some() && cand_sr == main_sr { score += 1; }
            if score > best_match.map(|(s, _)| s).unwrap_or(0) {
                best_match = Some((score, candidate));
            }
        }

        if let Some((score, path)) = best_match {
            if score >= 2 {
                Some(path.clone())
            } else {
                highest_fps.map(|(p, _)| p.clone())
            }
        } else {
            None
        }
    } else {
        subinput.clone()
    }
}

fn quote_filter_value(value: &str) -> String {
    format!("'{}'", escape_filter_value(value))
}

fn escape_filter_value(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '\\' | '\'' | ':' | ',' | '[' | ']' | ';' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        Args, active_preset, adopts_linear_prefix, encodes_in_chunks, media_bitrate_kbps,
        planner_encoder_config, quote_filter_value, scale_height,
    };
    use clap::Parser;

    fn parsed(extra: &[&str]) -> Args {
        let mut argv = vec!["pnmpeg", "--input", "in.mkv", "--output", "out.mp4"];
        argv.extend_from_slice(extra);
        Args::parse_from(argv)
    }

    // The encode worker spells its choice as `--preset <name>`; a hand-written run and the download
    // worker's speculative pass still spell it as a flag. Both have to reach the same preset, or
    // the two halves of one episode are encoded differently.
    #[test]
    fn a_flag_and_the_name_it_stands_for_select_the_same_preset() {
        for (flag, name) in [
            ("--x264", "standard"),
            ("--veryslow", "veryslow"),
            ("--pseudolossless", "pseudolossless"),
            ("--dummy", "dummy"),
            ("--gpu", "gpu"),
            ("--720p", "720p"),
            ("--480p", "480p"),
        ] {
            let by_flag = active_preset(&parsed(&[flag])).expect(flag);
            let by_name = active_preset(&parsed(&["--preset", name])).expect(name);
            assert_eq!(by_flag.name, by_name.name, "{flag} and --preset {name} disagree");
            assert_eq!(
                planner_encoder_config(Some(&by_flag)).crf,
                planner_encoder_config(Some(&by_name)).crf,
                "{flag} and --preset {name} would encode ahead at different CRFs",
            );
        }
    }

    // The regression this guards: the worker moved from `--x264` to `--preset standard`, and every
    // ahead-of-time decision was still reading the boolean flags. The encode kept working, so
    // nothing failed — it just silently stopped adopting the prefix the download worker had
    // already spent CPU producing.
    #[test]
    fn the_presets_that_encode_ahead_do_so_under_either_spelling() {
        for argv in [vec!["--x264"], vec!["--preset", "standard"], vec!["--pseudolossless"], vec![]] {
            let preset = active_preset(&parsed(&argv));
            assert!(adopts_linear_prefix(preset.as_ref()), "{argv:?} lost its AOT handoff");
            assert!(!encodes_in_chunks(preset.as_ref()), "{argv:?} should stay linear");
        }
        for argv in [vec!["--veryslow"], vec!["--preset", "veryslow"]] {
            let preset = active_preset(&parsed(&argv));
            assert!(encodes_in_chunks(preset.as_ref()), "{argv:?} lost parallel chunking");
            assert!(!adopts_linear_prefix(preset.as_ref()), "{argv:?} chunks instead of adopting");
        }
        // Not libx264, and a filter that must run exactly once in the encode that writes the file.
        for argv in [vec!["--gpu"], vec!["--preset", "720p"], vec!["--480p"]] {
            let preset = active_preset(&parsed(&argv));
            assert!(!adopts_linear_prefix(preset.as_ref()), "{argv:?} must not encode ahead");
            assert!(!encodes_in_chunks(preset.as_ref()), "{argv:?} must not chunk");
        }
        // A concat is not an encode and has nothing to adopt.
        for argv in [vec!["--concat"], vec!["--legacyconcat"]] {
            let preset = active_preset(&parsed(&argv));
            assert!(!adopts_linear_prefix(preset.as_ref()), "{argv:?} is not an encode");
            assert!(!encodes_in_chunks(preset.as_ref()), "{argv:?} is not an encode");
        }
    }

    // A concat stitches an intro onto an already-encoded file. It is not an encode, so it has no
    // preset and must never try to adopt a speculative prefix.
    #[test]
    fn a_concat_selects_no_preset_but_a_bare_run_defaults_to_standard() {
        assert!(active_preset(&parsed(&["--concat"])).is_none());
        assert!(active_preset(&parsed(&["--legacyconcat"])).is_none());

        // No flags at all is an encode, and it is the standard preset — the same one the parameter
        // selection falls back to, so the AOT settings cannot disagree with the ffmpeg run.
        let bare = active_preset(&parsed(&[])).expect("a bare run still encodes");
        assert_eq!(bare.name, "standard");
        assert_eq!(planner_encoder_config(Some(&bare)).crf, 17.0);
        assert_eq!(
            planner_encoder_config(Some(&bare)).x264_params.as_deref(),
            Some("aq-strength=0.8:aq-mode=3"),
            "the AOT encoder would have dropped the preset's x264 tuning",
        );
    }

    // The release is named after the height that was actually encoded, which only the preset's own
    // filter knows.
    #[test]
    fn the_output_height_follows_the_preset_that_scaled_it() {
        assert_eq!(scale_height(active_preset(&parsed(&["--preset", "720p"])).as_ref()), Some(720));
        assert_eq!(scale_height(active_preset(&parsed(&["--480p"])).as_ref()), Some(480));
        assert_eq!(scale_height(active_preset(&parsed(&["--x264"])).as_ref()), None);
    }

    #[test]
    fn quote_filter_value_escapes_filter_specials() {
        assert_eq!(
            quote_filter_value("C:\\work,subs\\a'b.ass"),
            "'C\\:\\\\work\\,subs\\\\a\\'b.ass'"
        );
    }

    #[test]
    fn bitrate_uses_media_time_instead_of_encoding_speed() {
        assert_eq!(media_bitrate_kbps(1_532_493_872, 878_493_493), 13_955);
        assert_eq!(media_bitrate_kbps(1_000, 0), 0);
    }
}
