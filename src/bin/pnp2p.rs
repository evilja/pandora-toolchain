use pandora_toolchain::libpnp2p::{core::*};
use pandora_toolchain::libpnprotocol::core::{Protocol, ToolInfo};

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
    magnet: bool,
    
    #[arg(long)]
    nomagnet: bool,

    #[arg(long)]
    opcode: String,

    #[arg(long)]
    save: String,

    #[arg(long)]
    negkey: Option<String>,

    #[arg(long)]
    negotiator: Option<String>,

    #[arg(long)]
    negver: Option<String>,

    #[arg(long)]
    cancelfile: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let mut proto = Protocol::new(vec![1]);
    let neg = proto.request(ToolInfo { tool: match args.negotiator {
                        Some(ref negotiator) => negotiator,
                        None => "PNp2p",
                    }, build: match args.negver {
                        Some(ref negver) => negver,
                        None => "0.1.1",
                    }, proto: 1 },
                  ToolInfo { tool: "PNp2p", build: "0.1.1", proto: 1 },
                  match args.negkey {
                      Some(key) => key,
                      None => "PNp2pCLI".to_string(),
                  });

    let p2pcp = P2p::new("admin", "adminadmin", args.cancelfile).await;

    p2pcp.download_and_remove(&args.opcode, &args.save, proto, neg, if args.nomagnet { false } else if args.magnet { true } else { false }).await.unwrap();

}
