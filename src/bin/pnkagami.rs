use clap::Parser;
use pandora_toolchain::libkagami::complex::overrides::ASSOverride;
use pandora_toolchain::libkagami::core::SubstationAlpha;
use pandora_toolchain::libkagami::tags::{ASSText, ASSLine};
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "pnkagami",
    version = "0.1.0",
    about = "Show libkagami advanced parsing changes",
    long_about = None
)]
struct Args {
    input: String,

    #[arg(long)]
    all: bool,

    #[arg(long)]
    no_pause: bool,
}

struct EventDiff {
    idx: usize,
    style: String,
    original: String,
    parsed: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let path = PathBuf::from(&args.input);
    let raw = SubstationAlpha::load(path.clone(), false).await;
    let adv = SubstationAlpha::load(path, true).await;

    let mut diffs = Vec::new();
    for (idx, (before, after)) in raw.events.iter().zip(adv.events.iter()).enumerate() {
        if is_drawing_line(&after.text) {
            continue;
        }
        let original = before.text.stringify();
        let parsed = after.text.stringify();
        if original == parsed && !args.all {
            continue;
        }
        diffs.push(EventDiff {
            idx: idx + 1,
            style: before.style.clone(),
            original,
            parsed,
        });
    }

    if diffs.is_empty() {
        println!("No advanced parsing changes.");
        return;
    }

    for (i, diff) in diffs.iter().enumerate() {
        print_event_diff(diff);
        if !args.no_pause && i + 1 < diffs.len() && !wait_for_next(i + 1, diffs.len()) {
            break;
        }
    }
}

fn is_drawing_line(line: &ASSLine) -> bool {
    line.data.iter().any(|item| matches!(item, ASSText::Override(ASSOverride::P(1))))
}

fn print_event_diff(diff: &EventDiff) {
    println!("\x1b[1;36mevent {}, style {}\x1b[0m", diff.idx, diff.style);
    if diff.original == diff.parsed {
        println!(" {}", diff.original);
    } else {
        println!("\x1b[31m-{}\x1b[0m", diff.original);
        println!("\x1b[32m+{}\x1b[0m", diff.parsed);
    }
}

fn wait_for_next(done: usize, total: usize) -> bool {
    print!("\n[{}/{}] Enter for next, q to quit: ", done, total);
    let _ = io::stdout().flush();
    let mut line = String::new();
    match io::stdin().read_line(&mut line) {
        Ok(_) => !line.trim().eq_ignore_ascii_case("q"),
        Err(_) => false,
    }
}
