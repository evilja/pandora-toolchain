use pandora_toolchain::libkagami::drawing::parse::Drawing;
use std::str::FromStr;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "pnass",
    version = "0.1.1",
    about = "Pandora Toolchain ASS tool",
    long_about = None
)]
struct Args {
    #[arg(short, long)]
    path: String,
    #[arg(short, long)]
    out: String,
}
// NOBORE! SUSUME! TAKAI TOU!
#[tokio::main]
async fn main() {

    let clip = "m 472 708 l 332 406 1224 342 1250 826 b 1055 694 665 431 470 300 558 278 869 564 824 212 985 260 1307 357 1468 406";
    let drawing = Drawing::from_str(clip).unwrap();
    println!("{:?}", drawing.commands);
}
