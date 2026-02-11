use pandora_toolchain::libpnp2p::core::*;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "pnp2p",
    version = "0.1.1",
    about = "Pandora Toolchain P2P wrapper",
    long_about = None
)]
struct Args {
    #[arg(long)]
    opcode: String, 

    #[arg(long)]
    save: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let p2pcp = P2p::new("admin", "adminadmin").await;
    if let Err(e) = p2pcp.download_and_remove(&args.opcode, &args.save).await {
        eprintln!("Error: {e}");
    }
}

