use pandora_toolchain::{libpncurl::core::{
        Req,
        RpbData,
        Host,
    },
};
use tokio::time::Instant;
use std::{path::PathBuf, time::Duration};
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

    #[arg(long)]
    logfile: Option<String>,
}

#[tokio::main]
async fn main() {
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
    let request = Req {
        target: args.link.clone(),
        log: args.logfile.map(PathBuf::from),
    };
    if !args.drive {
        request.send(args.opcode).await;
        println!("{}",
            pn_emit!(
                protocol = proto,
                negkey = &neg,
                schema = [leaf, leaf],
                data   = ["1", "DONE"]
            ).unwrap()
        )
    } else if let Some(a) = args.env {
        let (tx, rx): (Sender<RpbData>, Receiver<RpbData>) = mpsc::channel();

        let tx2 = tx.clone();
        let tx3 = tx.clone();
        let tx4 = tx.clone();
        let env2 = a.clone();
        let env3 = a.clone(); let env4 = a.clone();
        let opcode2 = args.opcode.clone();
        let opcode3 = args.opcode.clone(); let opcode4 = args.opcode.clone();
        let link2 = args.link.clone();
        let link3 = args.link.clone(); let link4 = args.link.clone();
        let log2 = request.log.clone();
        let log3 = request.log.clone(); let log4 = request.log.clone();

        tokio::spawn(async move {
            request.gdupload(a, Some(args.opcode), tx).await;
        });
        tokio::spawn(async move {
            let req2 = Req { target: link2, log: log2 };
            req2.doodwrapupload(env2, Some(opcode2), tx2).await;
        });
        tokio::spawn(async move {
            let req3 = Req { target: link3, log: log3 };
            req3.uqwrapupload(env3, Some(opcode3), tx3).await;
        });
        tokio::spawn(async move {
            let req3 = Req { target: link4, log: log4 };
            req3.luluwrapupload(env4, Some(opcode4), tx4).await;
        });

        let mut gd_done = 0u64;
        let mut gd_all = 0u64;
        let mut dood_done = 0u64;
        let mut dood_all = 0u64;
        let mut uq_done = 0u64;
        let mut uq_all = 0u64;
        let mut lulu_done = 0u64;
        let mut lulu_all = 0u64;
        let mut last = Instant::now();

        let mut gd_result: Option<Result<String, ()>> = None;
        let mut dood_result: Option<Result<String, ()>> = None;
        let mut uq_result: Option<Result<String, ()>> = None;
        let mut lulu_result: Option<Result<String, ()>> = None;

        while let Ok(val) = rx.recv() {
            match val {
                RpbData::Progress(done, total, Host::Drive) => {
                    gd_done = done; gd_all = total;
                    if last.elapsed() < Duration::from_secs(5) { continue; }
                    last = Instant::now();
                    println!("{}",
                        pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf]],
                            data   = ["0", [gd_done, gd_all], [dood_done, dood_all], [uq_done, uq_all], [lulu_done, lulu_all]]
                        ).unwrap()
                    );
                }
                RpbData::Progress(done, total, Host::Doodstream) => {
                    dood_done = done; dood_all = total;
                    if last.elapsed() < Duration::from_secs(5) { continue; }
                    last = Instant::now();
                    println!("{}",
                        pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf]],
                            data   = ["0", [gd_done, gd_all], [dood_done, dood_all], [uq_done, uq_all], [lulu_done, lulu_all]]
                        ).unwrap()
                    );
                }
                RpbData::Progress(done, total, Host::Uqload) => {
                    uq_done = done; uq_all = total;
                    if last.elapsed() < Duration::from_secs(5) { continue; }
                    last = Instant::now();
                    println!("{}",
                        pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf]],
                            data   = ["0", [gd_done, gd_all], [dood_done, dood_all], [uq_done, uq_all], [lulu_done, lulu_all]]
                        ).unwrap()
                    );
                }
                RpbData::Progress(done, total, Host::Lulu) => {
                    lulu_done = done; lulu_all = total;
                    if last.elapsed() < Duration::from_secs(5) { continue; }
                    last = Instant::now();
                    println!("{}",
                        pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf]],
                            data   = ["0", [gd_done, gd_all], [dood_done, dood_all], [uq_done, uq_all], [lulu_done, lulu_all]]
                        ).unwrap()
                    );
                }
                RpbData::Done(url, Host::Drive) => { gd_result = Some(Ok(url)); }
                RpbData::Done(url, Host::Doodstream) => { dood_result = Some(Ok(url)); }
                RpbData::Done(url, Host::Uqload) => { uq_result = Some(Ok(url)); }
                RpbData::Done(url, Host::Lulu) => { lulu_result = Some(Ok(url)); }
                RpbData::Fail(Host::Drive) => { gd_result = Some(Err(())); }
                RpbData::Fail(Host::Doodstream) => { dood_result = Some(Err(())); }
                RpbData::Fail(Host::Uqload) => { uq_result = Some(Err(())); }
                RpbData::Fail(Host::Lulu) => { lulu_result = Some(Err(())); }
            }

            if gd_result.is_some() && dood_result.is_some() && uq_result.is_some() && lulu_result.is_some() {
                let gd_str = match &gd_result { Some(Ok(url)) => url.as_str(), _ => "Başarısız" };
                let dood_str = match &dood_result { Some(Ok(url)) => url.as_str(), _ => "Başarısız" };
                let uq_str = match &uq_result { Some(Ok(url)) => url.as_str(), _ => "Başarısız" };
                let lulu_str = match &lulu_result { Some(Ok(url)) => url.as_str(), _ => "Başarısız" };
                println!("{}",
                    pn_emit!(
                        protocol = proto,
                        negkey = &neg,
                        schema = [leaf, leaf, leaf, leaf, leaf],
                        data   = ["1", gd_str, dood_str, uq_str, lulu_str]
                    ).unwrap()
                );
                break;
            }
        }
    }
}
