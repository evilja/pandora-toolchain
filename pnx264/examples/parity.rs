// Encodes a y4m through the FFI shim so the result can be compared byte-for-byte against the
// same encode driven by ffmpeg. Bit-identical output is the invariant every later stage
// (plan-emit, plan-replay, chunking) gets checked against, so it has to hold first.
//
//   cargo run --release --example parity -- <in.y4m> <out.264> <preset> <crf> [x264-params]

use pnx264::y4m::Y4mReader;
use std::env;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: parity <in.y4m> <out.264> <preset> <crf> [x264-params]");
        std::process::exit(2);
    }
    let src = BufReader::new(File::open(&args[1]).expect("open y4m"));
    let mut y4m = Y4mReader::new(src).expect("parse y4m header");
    let mut out = BufWriter::new(File::create(&args[2]).expect("create output"));

    let cfg = pnx264::Config {
        width: y4m.width as u32,
        height: y4m.height as u32,
        fps_num: y4m.fps_num,
        fps_den: y4m.fps_den,
        crf: args[4].parse().expect("crf"),
        threads: 1,
        preset: Some(args[3].clone()),
        profile: Some("high".into()),
        level: Some("4.1".into()),
        x264_params: args.get(5).cloned().filter(|s| !s.is_empty()),
        ..Default::default()
    };
    let mut enc = pnx264::Encoder::open(&cfg).expect("open encoder");

    let mut pts: i64 = 0;
    // x264's CLI leaves b_repeat_headers on for raw output, so SPS/PPS ride along with each
    // IDR and there is no separate header write to match.
    while let Some(f) = y4m.next_frame().expect("read frame") {
        let nals = enc
            .encode(f.y, f.u, f.v, f.stride_y, f.stride_c, f.stride_c, pts)
            .expect("encode");
        out.write_all(nals).expect("write");
        pts += 1;
    }
    loop {
        let nals = enc.flush().expect("flush");
        if nals.is_empty() {
            break;
        }
        out.write_all(nals).expect("write");
    }
    out.flush().expect("flush output");
    eprintln!("encoded {pts} frames");
}
