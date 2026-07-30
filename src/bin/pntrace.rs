use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use pandora_toolchain::lib::http::api::trace::standalone_router;

#[tokio::main]
async fn main() {
    let address = match parse_address() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("pntrace: {error}");
            eprintln!("usage: pntrace [--host 127.0.0.1] [--port 8788]");
            std::process::exit(2);
        }
    };
    let listener = match tokio::net::TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("pntrace: cannot bind {address}: {error}");
            std::process::exit(1);
        }
    };
    println!("Kagami Trace Lab: http://{address}");
    if let Err(error) = axum::serve(listener, standalone_router()).await {
        eprintln!("pntrace: {error}");
        std::process::exit(1);
    }
}

fn parse_address() -> Result<SocketAddr, String> {
    let mut host = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let mut port = 8788u16;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--host" => {
                host = args
                    .next()
                    .ok_or_else(|| "--host requires an address".to_string())?
                    .parse()
                    .map_err(|_| "--host is not a valid IP address".to_string())?;
            }
            "--port" => {
                port = args
                    .next()
                    .ok_or_else(|| "--port requires a number".to_string())?
                    .parse()
                    .map_err(|_| "--port is not a valid port".to_string())?;
            }
            "--help" | "-h" => {
                println!("usage: pntrace [--host 127.0.0.1] [--port 8788]");
                std::process::exit(0);
            }
            unknown => return Err(format!("unknown argument '{unknown}'")),
        }
    }
    Ok(SocketAddr::new(host, port))
}
