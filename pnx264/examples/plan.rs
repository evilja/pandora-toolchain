// Runs the fork's plan-only mode over a y4m and reports the frame types it decided, plus
// throughput. Two things are being checked:
//
//   1. Correctness — the IDR positions must match those a real encode chooses, since chunk
//      boundaries are cut there.
//   2. Speed — the planner has to outrun the chunk encoders it feeds, or the streaming design
//      in the notes does not hold and planning becomes a serial prepass that eats the latency
//      win it was meant to enable.
//
//   cargo run --release --example plan -- <in.y4m> <preset> <crf> [x264-params]

use pnx264::y4m::Y4mReader;
use std::env;
use std::fs::File;
use std::io::BufReader;
use std::time::Instant;

// x264's X264_TYPE_* values.
fn type_name(t: i32) -> &'static str {
    match t {
        1 => "IDR",
        2 => "I",
        3 => "P",
        4 => "BREF",
        5 => "B",
        _ => "?",
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: plan <in.y4m> <preset> <crf> [x264-params]");
        std::process::exit(2);
    }
    let src = BufReader::new(File::open(&args[1]).expect("open y4m"));
    let mut y4m = Y4mReader::new(src).expect("parse y4m header");

    let cfg = pnx264::Config {
        width: y4m.width as u32,
        height: y4m.height as u32,
        fps_num: y4m.fps_num,
        fps_den: y4m.fps_den,
        crf: args[3].parse().expect("crf"),
        // 0 = auto. Plan-only collapses coding threads to one internally and derives its
        // lookahead thread count from this, so asking for 1 here would serialise the only
        // part of planning that can be parallel.
        threads: env::var("PLAN_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
        preset: Some(args[2].clone()),
        profile: Some("high".into()),
        level: Some("4.1".into()),
        x264_params: args.get(4).cloned().filter(|s| !s.is_empty()),
        plan_only: true,
        ..Default::default()
    };
    let mut enc = pnx264::Encoder::open(&cfg).expect("open planner");

    let mut counts = [0usize; 8];
    let mut idrs: Vec<i64> = Vec::new();
    let mut planned = 0u64;
    let mut pts: i64 = 0;
    let start = Instant::now();
    let mut first_boundary_at = None;

    let record = |e: pnx264::PlanEntry, counts: &mut [usize; 8], idrs: &mut Vec<i64>| {
        let t = e.frame_type as usize;
        if t < counts.len() {
            counts[t] += 1;
        }
        if e.is_idr != 0 {
            idrs.push(e.pts);
        }
    };

    while let Some(f) = y4m.next_frame().expect("read frame") {
        if let Some(e) = enc
            .plan_push(f.y, f.u, f.v, f.stride_y, f.stride_c, f.stride_c, pts)
            .expect("plan push")
        {
            record(e, &mut counts, &mut idrs);
            planned += 1;
            // The latency that matters for the streaming design is not the whole pass, but how
            // long until the first chunk boundary is known and a worker can start.
            if idrs.len() == 2 && first_boundary_at.is_none() {
                first_boundary_at = Some(start.elapsed());
            }
        }
        pts += 1;
    }
    while let Some(e) = enc.plan_flush().expect("plan flush") {
        record(e, &mut counts, &mut idrs);
        planned += 1;
    }
    let elapsed = start.elapsed();

    println!("planned {planned} frames in {:.2}s = {:.1} fps", elapsed.as_secs_f64(),
             planned as f64 / elapsed.as_secs_f64());
    print!("frame types:");
    for (t, n) in counts.iter().enumerate() {
        if *n > 0 {
            print!(" {}={}", type_name(t as i32), n);
        }
    }
    println!();
    println!("IDR positions ({}): {:?}", idrs.len(), idrs);
    if let Some(d) = first_boundary_at {
        println!("first usable chunk boundary after {:.2}s", d.as_secs_f64());
    }
}
