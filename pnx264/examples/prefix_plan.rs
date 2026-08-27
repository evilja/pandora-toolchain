// Plans natural x264 IDR boundaries while a source is still downloading. The prefix sidecar is
// produced by Pandora's torrent/direct/Drive download paths; it is the authority because torrent
// files are preallocated and their apparent length includes bytes that have not arrived.
//
//   cargo run --release --example prefix_plan -- \
//     <download.prefix> <plan.txt> <subtitle.ass> <preset> <crf> [x264-params]

use pnx264::prefix;
use pnx264::y4m::Y4mReader;
use std::env;
use std::fs::OpenOptions;
use std::io::{BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

fn filter_quote(value: &str) -> String {
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
    format!("'{out}'")
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 6 {
        eprintln!(
            "usage: prefix_plan <download.prefix> <plan.txt> <subtitle.ass> <preset> <crf> [x264-params]"
        );
        std::process::exit(2);
    }
    let state_path = args[1].clone();
    let plan_path = args[2].clone();
    let subtitle = std::fs::canonicalize(&args[3]).unwrap_or_else(|_| args[3].clone().into());
    let filter = format!("ass={},format=yuv420p", filter_quote(&subtitle.to_string_lossy()));
    let ffmpeg = env::var("FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
    let mut child = Command::new(ffmpeg)
        .args([
            "-v", "error",
            "-i", "pipe:0",
            "-map", "0:v:0",
            "-an", "-sn",
            "-vf", &filter,
            "-pix_fmt", "yuv420p",
            "-f", "yuv4mpegpipe", "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn ffmpeg prefix decoder");
    let stdin = child.stdin.take().expect("ffmpeg stdin");
    let state_for_feeder = state_path.clone();
    let feeder = std::thread::spawn(move || prefix::stream_to(Path::new(&state_for_feeder), stdin));

    let stdout = child.stdout.take().expect("ffmpeg stdout");
    let mut y4m = Y4mReader::new(BufReader::with_capacity(1 << 20, stdout))
        .expect("read y4m header from growing source");
    let cfg = pnx264::Config {
        width: y4m.width as u32,
        height: y4m.height as u32,
        fps_num: y4m.fps_num,
        fps_den: y4m.fps_den,
        crf: args[5].parse().expect("crf"),
        // The fork derives lookahead threads before collapsing coding to one thread.
        threads: env::var("PLAN_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
        preset: Some(args[4].clone()),
        profile: Some("high".into()),
        level: Some("4.1".into()),
        x264_params: args.get(6).cloned().filter(|s| !s.is_empty()),
        plan_only: true,
        ..Default::default()
    };
    let mut encoder = pnx264::Encoder::open(&cfg).expect("open x264 planner");
    let mut plan = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&plan_path)
        .expect("create plan");
    plan.write_all(b"PNPLAN1\n").expect("write plan header");
    plan.flush().expect("flush plan header");

    let mut submitted = 0i64;
    let mut planned = 0u64;
    let mut last_planned_pts = 0u64;
    let mut idrs = 0u64;
    while let Some(frame) = y4m.next_frame().expect("decode growing source") {
        if let Some(entry) = encoder
            .plan_push(
                frame.y,
                frame.u,
                frame.v,
                frame.stride_y,
                frame.stride_c,
                frame.stride_c,
                submitted,
            )
            .expect("plan frame")
        {
            planned += 1;
            last_planned_pts = last_planned_pts.max(entry.pts.max(0) as u64);
            if entry.is_idr != 0 {
                writeln!(plan, "idr|{}", entry.pts).expect("write IDR");
                plan.flush().expect("publish IDR");
                idrs += 1;
            } else if planned.is_multiple_of(250) {
                writeln!(plan, "progress|{last_planned_pts}|{}", submitted + 1)
                    .expect("write plan progress");
                plan.flush().expect("publish plan progress");
            }
        }
        submitted += 1;
    }
    while let Some(entry) = encoder.plan_flush().expect("flush planner") {
        planned += 1;
        last_planned_pts = last_planned_pts.max(entry.pts.max(0) as u64);
        if entry.is_idr != 0 {
            writeln!(plan, "idr|{}", entry.pts).expect("write IDR");
            plan.flush().expect("publish IDR");
            idrs += 1;
        } else if planned.is_multiple_of(250) {
            writeln!(plan, "progress|{last_planned_pts}|{submitted}")
                .expect("write plan progress");
            plan.flush().expect("publish plan progress");
        }
    }
    writeln!(plan, "complete|{planned}|{submitted}").expect("finish plan");
    plan.flush().expect("flush completed plan");

    let fed = feeder.join().expect("prefix feeder panicked").expect("prefix feeder failed");
    let status = child.wait().expect("wait ffmpeg");
    if !status.success() {
        panic!("ffmpeg prefix decoder failed with {status}");
    }
    println!("planned {planned}/{submitted} frames, {idrs} IDRs from {fed} source bytes");
}
