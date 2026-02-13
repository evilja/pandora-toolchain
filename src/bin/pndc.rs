use serenity::{
    prelude::*,
    Client,
    all::{GatewayIntents, Message, Context},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::{self,
    sync::mpsc::{
        channel,
        Sender,
        Receiver,
    }
};
use pandora_toolchain::libpnenv::{
    core::get_env,
    standard::TOKEN
};
use pandora_toolchain::libpnmpeg::{
    core::{
        FFmpeg,
        FfmpegParams,
        do_encode
    },
    probe::ffprobe_lang,
    preset::{
        CPU_SANE_DEFAULTS,
        CPU_PSEUDOLOSSLESS,
        GPU_SANE_DEFAULTS,
    }
};
use pandora_toolchain::libpncurl::core::{
    Req,
    RpbData
};
use pandora_toolchain::libpnp2p::core::P2p;
use pandora_toolchain::pnworker::core::{Job, Handler};


#[tokio::main]
async fn main () {
    let env = get_env("env.pandora".into());
    let (tx, rx): (Sender<Job>, Receiver<Job>) = channel(5);
    let mut discord = Client::builder(env[TOKEN].clone(), GatewayIntents::all()).event_handler(Handler { tx: tx }).await.unwrap();
    if let Err(why) = discord.start().await {
        println!("{}", why);
    }
}
