use pandora_toolchain::{libpncurl::core::{
        Req,
        RpbData
    },
};
use std::thread::{self};
use clap::Parser;
use std::sync::mpsc::{Sender, Receiver, self};
use pandora_toolchain::{pn_data, pn_emit, pn_schema};
use pandora_toolchain::libpnprotocol::core::{Protocol, Schema, ToolInfo};


#[derive(Parser, Debug)]
#[command(
    name = "pncurl",
    version = "0.1.1",
    about = "Pandora Toolchain CURL wrapper",
    long_about = None
)]
struct Args {
    #[arg(short, long)]
    link: String,

    #[arg(short, long)]
    opcode: String,

    #[arg(short, long)]
    env: Option<String>,

    #[arg(long)]
    negkey: Option<String>,

    #[arg(long)]
    negotiator: Option<String>,

    #[arg(long)]
    negver: Option<String>,

    #[arg(short, long)]
    drive: bool,
}

fn main() {
    let args = Args::parse();
    let mut proto = Protocol::new(vec![1]);
    let neg = proto.request(ToolInfo { tool: match args.negotiator {
                        Some(ref negotiator) => negotiator,
                        None => "PNcurl",
                    }, build: match args.negver {
                        Some(ref negver) => negver,
                        None => "0.1.1",
                    }, proto: 1 },
                  ToolInfo { tool: "PNcurl", build: "0.1.1", proto: 1 },
                  match args.negkey {
                      Some(key) => key,
                      None => "PNcurlCLI".to_string(),
                  });

    if !args.drive {
        let request = Req {
            target: args.link
        };

        request.send(args.opcode);
        println!("{}",
            pn_emit!(
                protocol = proto,
                negkey = &neg,
                schema = [leaf, leaf],
                data   = ["1", "DONE"]
            ).unwrap()
        )
    } else if let Some(a) = args.env {
        let request = Req {
            target: args.link
        };
        let (tx, rx): (Sender<RpbData>, Receiver<RpbData>) = mpsc::channel();
        let _thr = thread::spawn(move || {
            request.gdupload(a, Some(args.opcode), "1QBkY2JUV63lfD0c12SxE-eHmpWy3JPkD", tx)
        });

        while let Ok(val) = rx.recv() {
            match val {
                RpbData::Progress(done, total) => {
                    println!("{}",
                        pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, [leaf, leaf]],
                            data   = ["0", [done, total]]
                        ).unwrap()
                    )
                }
                RpbData::Done(a) => {
                    println!("{}",
                        pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, leaf],
                            data   = ["1", a]
                        ).unwrap()
                    )
                }
                RpbData::Fail => {
                    println!("{}",
                        pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, leaf],
                            data   = ["2", "0"]
                        ).unwrap()
                    )
                }
            }
        }
    }
}
