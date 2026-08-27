// Parallel chunked encode. Each chunk pulls its own frames from a dedicated ffmpeg process
// rather than from a shared decoded file: a 24-minute episode as y4m would be ~107 GB, and the
// production pipeline has to decode (and later burn subtitles) per chunk regardless.
//
// Every chunk starts on an IDR — x264 always begins that way — and all chunks share one
// SPS/PPS, so the elementary streams concatenate directly with no renumbering.
//
//   cargo run --release --example chunked -- <src> <out.264> <preset> <crf> <stride> <workers> [x264-params]

use pnx264::y4m::Y4mReader;
use std::env;
use std::fs::File;
use std::io::{BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

struct Source {
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    frames: u64,
}

fn probe(path: &str) -> Result<Source, String> {
    let out = Command::new("ffprobe")
        .args([
            "-v", "error", "-select_streams", "v:0",
            "-count_frames",
            "-show_entries", "stream=width,height,r_frame_rate,nb_read_frames",
            "-of", "default=nw=1:nk=1", path,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let f: Vec<&str> = text.split_whitespace().collect();
    if f.len() < 4 {
        return Err(format!("ffprobe gave {f:?}"));
    }
    let (n, d) = f[2].split_once('/').ok_or("bad r_frame_rate")?;
    Ok(Source {
        width: f[0].parse().map_err(|_| "bad width")?,
        height: f[1].parse().map_err(|_| "bad height")?,
        fps_num: n.parse().map_err(|_| "bad fps num")?,
        fps_den: d.parse().map_err(|_| "bad fps den")?,
        frames: f[3].parse().map_err(|_| "bad frame count")?,
    })
}

fn encode_chunk(src: &str, start: u64, frames: u64, s: &Source, cfg: &pnx264::Config) -> Result<Vec<u8>, String> {
    // Input-side -ss is frame accurate in modern ffmpeg: it seeks to the preceding keyframe and
    // then decodes and discards up to the requested timestamp, so it is both fast and exact.
    let ss = format!("{}", start as f64 * s.fps_den as f64 / s.fps_num as f64);
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error", "-ss", &ss, "-i", src,
            "-frames:v", &frames.to_string(),
            "-an", "-sn", "-pix_fmt", "yuv420p", "-f", "yuv4mpegpipe", "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn ffmpeg: {e}"))?;

    let stdout = child.stdout.take().ok_or("no ffmpeg stdout")?;
    let mut y4m = Y4mReader::new(BufReader::with_capacity(1 << 20, stdout))?;
    let mut enc = pnx264::Encoder::open(cfg)?;
    let mut out = Vec::new();
    let mut n = 0u64;
    // pts restarts per chunk: each chunk is an independent stream, retimed at mux.
    while n < frames {
        let Some(f) = y4m.next_frame()? else { break };
        out.extend_from_slice(enc.encode(
            f.y, f.u, f.v, f.stride_y, f.stride_c, f.stride_c, n as i64,
        )?);
        n += 1;
    }
    loop {
        let nals = enc.flush()?;
        if nals.is_empty() {
            break;
        }
        out.extend_from_slice(nals);
    }
    let _ = child.wait();
    if n != frames {
        return Err(format!("chunk at {start} wanted {frames} frames, ffmpeg gave {n}"));
    }
    Ok(out)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 7 {
        eprintln!("usage: chunked <src> <out.264> <preset> <crf> <stride> <workers> [x264-params]");
        std::process::exit(2);
    }
    let (src, output, preset) = (args[1].clone(), args[2].clone(), args[3].clone());
    let crf: f32 = args[4].parse().expect("crf");
    let stride: u64 = args[5].parse().expect("stride");
    let workers: usize = args[6].parse().expect("workers");
    let x264_params = args.get(7).cloned().filter(|s| !s.is_empty());

    let s = Arc::new(probe(&src).expect("probe source"));
    let mut ranges = Vec::new();
    let mut start = 0u64;
    while start < s.frames {
        let n = stride.min(s.frames - start);
        ranges.push((start, n));
        start += n;
    }
    // Chunk count must comfortably exceed worker count or the tail dominates wall time: with
    // 6 chunks on 12 workers, half the machine idles and one full chunk sets the floor.
    eprintln!(
        "{} frames -> {} chunks of <={stride} across {workers} workers ({:.1} chunks/worker)",
        s.frames, ranges.len(), ranges.len() as f64 / workers as f64
    );

    let cfg = pnx264::Config {
        width: s.width,
        height: s.height,
        fps_num: s.fps_num,
        fps_den: s.fps_den,
        crf,
        threads: 1,
        preset: Some(preset),
        profile: Some("high".into()),
        level: Some("4.1".into()),
        x264_params,
        ..Default::default()
    };

    let ranges = Arc::new(ranges);
    let next = Arc::new(AtomicUsize::new(0));
    let results: Arc<Vec<std::sync::Mutex<Option<Vec<u8>>>>> =
        Arc::new((0..ranges.len()).map(|_| std::sync::Mutex::new(None)).collect());

    let t0 = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let (ranges, next, results, cfg, src, s) =
                (ranges.clone(), next.clone(), results.clone(), cfg.clone(), src.clone(), s.clone());
            scope.spawn(move || loop {
                // Shared counter, not a static split: chunk cost varies with content, so
                // handing out the next outstanding chunk keeps the tail balanced.
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(&(start, n)) = ranges.get(i) else { return };
                let data = encode_chunk(&src, start, n, &s, &cfg)
                    .unwrap_or_else(|e| panic!("chunk {i} at {start}: {e}"));
                *results[i].lock().unwrap() = Some(data);
            });
        }
    });
    let elapsed = t0.elapsed();

    let mut out = File::create(&output).expect("create output");
    let mut written = 0usize;
    for slot in results.iter() {
        let data = slot.lock().unwrap().take().expect("chunk missing");
        written += data.len();
        out.write_all(&data).expect("write");
    }
    out.flush().expect("flush");
    println!(
        "chunked: {} frames in {:.2}s = {:.2} fps ({} chunks, {workers} workers, {written} bytes)",
        s.frames, elapsed.as_secs_f64(), s.frames as f64 / elapsed.as_secs_f64(), ranges.len()
    );
}
