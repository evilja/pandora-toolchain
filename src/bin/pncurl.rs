use pandora_toolchain::{lib::http::curl::core::{
        Req,
        RpbData,
        Host,
        DownloadStatus,
    },
    lib::http::curl::gscrape::GScrape,
};
use tokio::time::Instant;
use std::{path::PathBuf, time::Duration};
use clap::Parser;
use tokio::sync::mpsc::{UnboundedSender, UnboundedReceiver, self};
use pandora_toolchain::{pn_data, pn_emit, pn_schema};
use pandora_toolchain::lib::protocol::core::{Protocol, Schema, ToolInfo};


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

    #[arg(long)]
    drive_opcode: Option<String>,

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

    #[arg(long)]
    cancelfile: Option<String>,

    #[arg(long)]
    backup: bool,

    #[arg(long)]
    gscrape: bool,

    #[arg(long)]
    drive_folder: Option<String>,
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
        log: args.logfile.clone().map(PathBuf::from),
        cfile: args.cancelfile.clone().map(PathBuf::from),
    };
    if args.gscrape {
        let scraper = GScrape {
            link: args.link.clone(),
            log: args.logfile.map(PathBuf::from),
            cfile: args.cancelfile.map(PathBuf::from),
        };
        let ok = scraper.send(args.opcode, &proto, &neg).await;
        let code = if ok { "1" } else { "2" };
        let msg = if ok { "DONE" } else { "FAIL" };
        println!("{}",
            pn_emit!(
                protocol = proto,
                negkey = &neg,
                schema = [leaf, leaf],
                data   = [code, msg]
            ).unwrap()
        )
    } else if !args.drive {
        let status = request
            .send_with_progress(args.opcode, Some(&proto), Some(&neg))
            .await;
        let (code, msg, exit_failed) = match status {
            DownloadStatus::Success => ("1", "DONE", false),
            DownloadStatus::Fail => ("2", "FAIL", true),
            DownloadStatus::Cancel => ("3", "CANCELLED", false),
        };
        println!("{}",
            pn_emit!(
                protocol = proto,
                negkey = &neg,
                schema = [leaf, leaf],
                data   = [code, msg]
            ).unwrap()
        );
        if exit_failed {
            std::process::exit(1);
        }
    } else if let Some(a) = args.env {
        let (tx, mut rx): (UnboundedSender<RpbData>, UnboundedReceiver<RpbData>) = mpsc::unbounded_channel();

        let tx2 = tx.clone();
        let tx4 = tx.clone(); let tx5 = tx.clone(); let tx6 = tx.clone();
        let env2 = a.clone();
        let env4 = a.clone(); let env5 = a.clone(); let env6 = a.clone();
        let opcode2 = args.opcode.clone();
        let opcode4 = args.opcode.clone(); let opcode5 = args.opcode.clone(); let opcode6 = args.opcode.clone();
        let link2 = args.link.clone();
        let link4 = args.link.clone(); let link5 = args.link.clone(); let link6 = args.link.clone();
        let log2 = request.log.clone();
        let log4 = request.log.clone(); let log5 = request.log.clone(); let log6 = request.log.clone();
        let cfile2 = request.cfile.clone();
        let cfile4 = request.cfile.clone(); let cfile5 = request.cfile.clone(); let cfile6 = request.cfile.clone();

        let drive_folder = args.drive_folder.clone();
        let drive_opcode = args.drive_opcode.clone().unwrap_or_else(|| args.opcode.clone());
        tokio::spawn(async move {
            request.gdupload(a, Some(drive_opcode), drive_folder, tx).await;
        });
        if !args.backup {
            tokio::spawn(async move {
                let req2 = Req { target: link2, log: log2, cfile: cfile2 };
                req2.doodwrapupload(env2, Some(opcode2), tx2).await;
            });
            tokio::spawn(async move {
                let req4 = Req { target: link4, log: log4, cfile: cfile4 };
                req4.luluwrapupload(env4, Some(opcode4), tx4).await;
            });
            tokio::spawn(async move {
                let req5 = Req { target: link5, log: log5, cfile: cfile5 };
                req5.voewrapupload(env5, Some(opcode5), tx5).await;
            });
            tokio::spawn(async move {
                let req6 = Req { target: link6, log: log6, cfile: cfile6 };
                req6.abyssupload(env6, Some(opcode6), tx6).await;
            });
        }

        let mut total_size = 0u64;
        let mut gd_done = 0u64;
        let mut gd_ext = 0u64;
        let mut dood_done = 0u64;
        let mut dood_ext = 0u64;
        let mut lulu_done = 0u64;
        let mut lulu_ext = 0u64;
        let mut voesx_done = 0u64;
        let mut voesx_ext = 0u64;
        let mut abyss_done = 0u64;
        let mut abyss_ext = 0u64;
        let mut last: Option<Instant> = None;

        let mut gd_result: Option<Result<String, ()>> = None;
        let mut dood_result: Option<Result<String, ()>> = None;
        let mut lulu_result: Option<Result<String, ()>> = None;
        let mut voesx_result: Option<Result<String, ()>> = None;
        let mut abyss_result: Option<Result<String, ()>> = None;

        while let Some(val) = rx.recv().await {
            match val {
                RpbData::Progress(done, total, extensions, Host::Drive) => {
                    if total != 0 { total_size = total; }
                    gd_done = done; gd_ext = extensions;
                    if last.map(|t| t.elapsed() < Duration::from_secs(5)).unwrap_or(false) { continue; }
                    last = Some(Instant::now());
                    println!("{}",
                        pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, leaf, [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf]],
                            data   = ["0", total_size, [gd_done, gd_ext], [dood_done, dood_ext], [lulu_done, lulu_ext], [voesx_done, voesx_ext], [abyss_done, abyss_ext]]
                        ).unwrap()
                    );
                }
                RpbData::Progress(done, total, extensions, Host::Doodstream) => {
                    if total != 0 { total_size = total; }
                    dood_done = done; dood_ext = extensions;
                    if last.map(|t| t.elapsed() < Duration::from_secs(5)).unwrap_or(false) { continue; }
                    last = Some(Instant::now());
                    println!("{}",
                        pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, leaf, [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf]],
                            data   = ["0", total_size, [gd_done, gd_ext], [dood_done, dood_ext], [lulu_done, lulu_ext], [voesx_done, voesx_ext], [abyss_done, abyss_ext]]
                        ).unwrap()
                    );
                }
                RpbData::Progress(done, total, extensions, Host::Lulu) => {
                    if total != 0 { total_size = total; }
                    lulu_done = done; lulu_ext = extensions;
                    if last.map(|t| t.elapsed() < Duration::from_secs(5)).unwrap_or(false) { continue; }
                    last = Some(Instant::now());
                    println!("{}",
                        pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, leaf, [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf]],
                            data   = ["0", total_size, [gd_done, gd_ext], [dood_done, dood_ext], [lulu_done, lulu_ext], [voesx_done, voesx_ext], [abyss_done, abyss_ext]]
                        ).unwrap()
                    );
                }
                RpbData::Progress(done, total, extensions, Host::VoeSx) => {
                    if total != 0 { total_size = total; }
                    voesx_done = done; voesx_ext = extensions;
                    if last.map(|t| t.elapsed() < Duration::from_secs(5)).unwrap_or(false) { continue; }
                    last = Some(Instant::now());
                    println!("{}",
                        pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, leaf, [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf]],
                            data   = ["0", total_size, [gd_done, gd_ext], [dood_done, dood_ext], [lulu_done, lulu_ext], [voesx_done, voesx_ext], [abyss_done, abyss_ext]]
                        ).unwrap()
                    );
                }
                RpbData::Progress(done, total, extensions, Host::Abyss) => {
                    if total != 0 { total_size = total; }
                    abyss_done = done; abyss_ext = extensions;
                    if last.map(|t| t.elapsed() < Duration::from_secs(5)).unwrap_or(false) { continue; }
                    last = Some(Instant::now());
                    println!("{}",
                        pn_emit!(
                            protocol = proto,
                            negkey = &neg,
                            schema = [leaf, leaf, [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf], [leaf, leaf]],
                            data   = ["0", total_size, [gd_done, gd_ext], [dood_done, dood_ext], [lulu_done, lulu_ext], [voesx_done, voesx_ext], [abyss_done, abyss_ext]]
                        ).unwrap()
                    );
                }
                RpbData::Done(url, Host::Drive, folder_id) => {
                    gd_result = Some(Ok(url.clone()));
                    let folder_id = folder_id.unwrap_or_default();
                    println!("{}", pn_emit!(protocol = proto, negkey = &neg,
                        schema = [leaf, leaf, leaf, leaf], data = ["1", "1", url, folder_id]).unwrap());
                }
                RpbData::Done(url, Host::Doodstream, _) => {
                    dood_result = Some(Ok(url.clone()));
                    println!("{}", pn_emit!(protocol = proto, negkey = &neg,
                        schema = [leaf, leaf, leaf], data = ["1", "2", url]).unwrap());
                }
                RpbData::Done(url, Host::Lulu, _) => {
                    lulu_result = Some(Ok(url.clone()));
                    println!("{}", pn_emit!(protocol = proto, negkey = &neg,
                        schema = [leaf, leaf, leaf], data = ["1", "4", url]).unwrap());
                }
                RpbData::Done(url, Host::VoeSx, _) => {
                    voesx_result = Some(Ok(url.clone()));
                    println!("{}", pn_emit!(protocol = proto, negkey = &neg,
                        schema = [leaf, leaf, leaf], data = ["1", "5", url]).unwrap());
                }
                RpbData::Done(url, Host::Abyss, _) => {
                    abyss_result = Some(Ok(url.clone()));
                    println!("{}", pn_emit!(protocol = proto, negkey = &neg,
                        schema = [leaf, leaf, leaf], data = ["1", "6", url]).unwrap());
                }
                RpbData::Fail(Host::Drive) => { gd_result = Some(Err(())); }
                RpbData::Fail(Host::Doodstream) => { dood_result = Some(Err(())); }
                RpbData::Fail(Host::Lulu) => { lulu_result = Some(Err(())); }
                RpbData::Fail(Host::VoeSx) => { voesx_result = Some(Err(())); }
                RpbData::Fail(Host::Abyss) => { abyss_result = Some(Err(())); }
                RpbData::Cancel(_) => {
                    println!("{}", pn_emit!(protocol = proto, negkey = &neg,
                        schema = [leaf, leaf], data = ["3", "CANCELLED"]).unwrap());
                    break;
                }
            }

            if (args.backup && gd_result.is_some()) || (gd_result.is_some() && dood_result.is_some() &&  voesx_result.is_some() && lulu_result.is_some() && abyss_result.is_some()) {
                break;
            }
        }
    }
}
