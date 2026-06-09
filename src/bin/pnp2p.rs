use pandora_toolchain::libpnp2p::{core::*};
use pandora_toolchain::libpnprotocol::core::{Protocol, ToolInfo};
use pandora_toolchain::{pn_emit, pn_data, pn_schema};
use pandora_toolchain::libpnprotocol::core::*;

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
    save: Option<String>,

    #[arg(long)]
    negkey: Option<String>,

    #[arg(long)]
    negotiator: Option<String>,

    #[arg(long)]
    negver: Option<String>,

    #[arg(long)]
    cancelfile: Option<String>,

    #[arg(long)]
    probe: bool,

    #[arg(long)]
    select: Option<u64>,  // file index chosen by user
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

    if args.probe {
        // probe mode: list mkv files, emit them as protocol output
        let files = p2pcp.probe_torrent(&args.opcode, !args.nomagnet && args.magnet).await.unwrap();
        for (idx, name, size) in files {
            println!("{}", pn_emit!(
                protocol = proto,
                negkey = &neg,
                schema = [leaf, [leaf, leaf, leaf]],
                data = ["4", [idx, name, size]]   // opcode 4 = probe result row
            ).unwrap());
        }
        println!("{}", pn_emit!(
            protocol = proto,
            negkey = &neg,
            schema = [leaf, leaf],
            data = ["1", "DONE"]
        ).unwrap());
        return;
    }
    
    let result = if let Some(index) = args.select {
        p2pcp.download_selected(
            &args.opcode, &args.save.unwrap(),
            vec![index], proto, neg, !args.nomagnet && args.magnet
        ).await
    } else {
        p2pcp.download_and_remove(&args.opcode, &args.save.unwrap(), proto, neg, if args.nomagnet { false } else if args.magnet { true } else { false }).await
    };

    if let Err(e) = result {
        eprintln!("[pnp2p] failed: {}", e);
        std::process::exit(1);
    }

}
