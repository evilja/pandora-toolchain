use pandora_toolchain::libpnp2p::core::*;
use pandora_toolchain::libpnprotocol::core::*;
use pandora_toolchain::libpnprotocol::core::{Protocol, ToolInfo};
use pandora_toolchain::{pn_data, pn_emit, pn_schema};

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
    select: Option<u64>, // file index chosen by user

    #[arg(long)]
    tag: Option<String>,
}

fn emit_error(proto: &Protocol, neg: &str, err: &str) {
    if err.contains("DUPLICATE_TORRENT") {
        let save_path = err.split_once('|').map(|(_, path)| path).unwrap_or("");
        println!(
            "{}",
            pn_emit!(
                protocol = proto,
                negkey = neg,
                schema = [leaf, leaf, leaf],
                data = ["5", "DUPLICATE_TORRENT", save_path]
            )
            .unwrap()
        );
        return;
    }
    println!(
        "{}",
        pn_emit!(
            protocol = proto,
            negkey = neg,
            schema = [leaf, leaf],
            data = ["2", "ERROR"]
        )
        .unwrap()
    );
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let mut proto = Protocol::new(vec![1]);
    let neg = proto.request(
        ToolInfo {
            tool: match args.negotiator {
                Some(ref negotiator) => negotiator,
                None => "PNp2p",
            },
            build: match args.negver {
                Some(ref negver) => negver,
                None => "0.1.1",
            },
            proto: 1,
        },
        ToolInfo {
            tool: "PNp2p",
            build: "0.1.1",
            proto: 1,
        },
        match args.negkey {
            Some(key) => key,
            None => "PNp2pCLI".to_string(),
        },
    );

    let qbit_host =
        std::env::var("PNP2P_QBIT_HOST").unwrap_or_else(|_| "http://localhost:8089".to_string());
    let qbit_user = std::env::var("PNP2P_QBIT_USER").unwrap_or_else(|_| "admin".to_string());
    let qbit_pass = std::env::var("PNP2P_QBIT_PASS").unwrap_or_else(|_| "adminadmin".to_string());
    let p2pcp = P2p::new(&qbit_host, &qbit_user, &qbit_pass, args.cancelfile).await;

    if args.probe {
        // probe mode: list mkv files, emit them as protocol output
        let files = match p2pcp
            .probe_torrent(
                &args.opcode,
                !args.nomagnet && args.magnet,
                args.tag.clone(),
            )
            .await
        {
            Ok(files) => files,
            Err(e) => {
                emit_error(&proto, &neg, &e.to_string());
                std::process::exit(1);
            }
        };
        for (idx, name, size) in files {
            println!(
                "{}",
                pn_emit!(
                    protocol = proto,
                    negkey = &neg,
                    schema = [leaf, [leaf, leaf, leaf]],
                    data = ["4", [idx, name, size]] // opcode 4 = probe result row
                )
                .unwrap()
            );
        }
        println!(
            "{}",
            pn_emit!(
                protocol = proto,
                negkey = &neg,
                schema = [leaf, leaf],
                data = ["1", "DONE"]
            )
            .unwrap()
        );
        return;
    }

    let result = if let Some(index) = args.select {
        p2pcp
            .download_selected(
                &args.opcode,
                &args.save.unwrap(),
                vec![index],
                &proto,
                neg.clone(),
                !args.nomagnet && args.magnet,
                args.tag.clone(),
            )
            .await
    } else {
        p2pcp
            .download_and_remove(
                &args.opcode,
                &args.save.unwrap(),
                &proto,
                neg.clone(),
                if args.nomagnet {
                    false
                } else if args.magnet {
                    true
                } else {
                    false
                },
                args.tag.clone(),
            )
            .await
    };

    if let Err(e) = result {
        emit_error(&proto, &neg, &e.to_string());
        eprintln!("[pnp2p] failed: {}", e);
        std::process::exit(1);
    }
}
