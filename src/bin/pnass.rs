use clap::Parser;
use pandora_toolchain::libkagami::core::{ScriptInfo, SubstationAlpha};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "pnass",
    version = "0.1.1",
    about = "Pandora Toolchain ASS standardiser",
    long_about = None
)]
struct Args {
    #[arg(short, long)]
    path: String,
    #[arg(short, long)]
    out: String,
    #[arg(short, long)]
    title: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let mut sub = SubstationAlpha::load(PathBuf::from(&args.path), false).await;

    sub.script_info = ScriptInfo {
        title: args.title,
        script_type: "v4.00+".to_string(),
        wrap_style: 2,
        scaled_border_and_shadow: true,
        playresx: 1920,
        playresy: 1080,
        ycbcr_matrix: "TV.709".to_string(),
        layout_res_x: 1920,
        layout_res_y: 1080,
    };

    if let Err(()) = sub.dump_to_file(PathBuf::from(&args.out)).await {
        eprintln!("pnass: failed to write {}", args.out);
    }
}
