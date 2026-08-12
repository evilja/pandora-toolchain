use pandora_toolchain::lib::p2p::core::*;
use pandora_toolchain::lib::protocol::core::*;
use pandora_toolchain::lib::protocol::core::{Protocol, ToolInfo};
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

    // Batch selection: every listed index is downloaded by this one process, and each file is
    // announced with opcode 6 as soon as its last piece lands.
    #[arg(long)]
    selects: Option<String>,

    #[arg(long)]
    tag: Option<String>,
}

// `--select` stays a single index for the existing worker call; `--selects` adds the comma list a
// batch uses. Duplicates are dropped so one file is never scheduled twice.
fn parse_selection(select: Option<u64>, selects: Option<&str>) -> Vec<u64> {
    let mut indices: Vec<u64> = select.into_iter().collect();
    for value in selects.unwrap_or("").split(',') {
        let Ok(index) = value.trim().parse::<u64>() else {
            continue;
        };
        if !indices.contains(&index) {
            indices.push(index);
        }
    }
    indices
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

    let p2pcp = match P2p::new(args.cancelfile).await {
        Ok(client) => client,
        Err(error) => {
            emit_error(&proto, &neg, &error.to_string());
            eprintln!("[pnp2p] initialization failed: {error}");
            std::process::exit(1);
        }
    };

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

    let selection = parse_selection(args.select, args.selects.as_deref());
    let result = if !selection.is_empty() {
        p2pcp
            .download_selected(
                &args.opcode,
                &args.save.unwrap(),
                selection,
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

#[cfg(test)]
mod tests {
    use super::parse_selection;

    #[test]
    fn a_single_select_still_downloads_one_file() {
        assert_eq!(parse_selection(Some(4), None), vec![4]);
        assert!(parse_selection(None, None).is_empty());
    }

    #[test]
    fn batch_selection_parses_a_list_and_drops_duplicates_and_junk() {
        assert_eq!(
            parse_selection(None, Some("3, 4,4, x, 9")),
            vec![3, 4, 9]
        );
        assert_eq!(parse_selection(Some(3), Some("3,5")), vec![3, 5]);
    }
}
