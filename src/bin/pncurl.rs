use pandora_toolchain::libpncurl::{
    core::{
        Req
    },
};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "pncurl",
    version = "0.1.1",
    about = "Pandora Toolchain CURL wrapper",
    long_about = None
)]
struct Args {
    #[arg(long)]
    link: String,

    #[arg(long)]
    opcode: String, 
}

fn main() {
    let args = Args::parse();
    let request = Req {
        target: args.link
    };

    request.send(args.opcode);  
}