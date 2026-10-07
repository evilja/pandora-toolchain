// Worker-owned proxy health: each torrent subprocess keeps the routing it started with.
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::lib::torrent::ProxyConfig;
use tokio::process::Command;
use tokio::task::JoinHandle;

static USE_PROXY: AtomicBool = AtomicBool::new(false);
const DEFAULT_URL: &str = "https://nyaa.si/";
const PROXY_KEYS: [&str; 3] = ["PNP2P_PROXY", "ALL_PROXY", "all_proxy"];

pub(crate) struct Monitor(Option<JoinHandle<()>>);

impl Drop for Monitor {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

// Complete the first check before workers can launch a torrent tool. An untested or invalid
// proxy is bypassed, and a failed check stays bypassed until a later check succeeds.
pub(crate) async fn start() -> Monitor {
    USE_PROXY.store(false, Ordering::Relaxed);
    let proxy = match ProxyConfig::from_env() {
        Ok(Some(proxy)) => proxy,
        Ok(None) => return Monitor(None),
        Err(_) => {
            eprintln!(
                "[Pandora proxy] invalid proxy configuration; torrent tools will connect directly"
            );
            return Monitor(None);
        }
    };
    let url = std::env::var("PNP2P_PROXY_CHECK_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    let interval = std::env::var("PNP2P_PROXY_CHECK_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(60);
    let client = reqwest::Proxy::all(proxy.reqwest_url()).and_then(|proxy| {
        reqwest::Client::builder()
            .no_proxy()
            .proxy(proxy)
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(10))
            .build()
    });
    let Ok(client) = client else {
        eprintln!(
            "[Pandora proxy] health client could not start; torrent tools will connect directly"
        );
        return Monitor(None);
    };
    let healthy = probe(&client, &url).await;
    publish(healthy, true);
    Monitor(Some(tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            publish(probe(&client, &url).await, false);
        }
    })))
}

async fn probe(client: &reqwest::Client, url: &str) -> bool {
    // Never retry directly here: a direct response says nothing about the proxy. Do not print
    // reqwest errors, which can contain configured URLs or authentication information.
    match client.get(url).send().await {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

fn publish(healthy: bool, initial: bool) {
    let previous = USE_PROXY.swap(healthy, Ordering::Relaxed);
    if initial || previous != healthy {
        eprintln!(
            "[Pandora proxy] {}; new torrent tools will connect {}",
            if healthy {
                "health check passed"
            } else {
                "health check failed"
            },
            if healthy {
                "through the proxy"
            } else {
                "directly until a health check passes"
            }
        );
    }
}

pub(crate) fn apply_to_command(command: &mut Command) {
    apply_routing(command, USE_PROXY.load(Ordering::Relaxed));
}

fn apply_routing(command: &mut Command, healthy: bool) {
    if !healthy {
        // Clearing only PNP2P_PROXY would let ProxyConfig fall back to ALL_PROXY.
        for key in PROXY_KEYS {
            command.env_remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn health_follows_proxy_failure_and_recovery_without_direct_fallback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for status in ["200 OK", "503 Service Unavailable", "200 OK"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 4096];
                let count = stream.read(&mut bytes).await.unwrap();
                assert!(
                    String::from_utf8_lossy(&bytes[..count])
                        .starts_with("GET http://health.invalid/ HTTP/1.1")
                );
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::all(proxy_url).unwrap())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        for healthy in [true, false, true] {
            assert_eq!(probe(&client, "http://health.invalid/").await, healthy);
            let mut command = Command::new("pnp2p");
            apply_routing(&mut command, healthy);
            let removed: Vec<_> = command
                .as_std()
                .get_envs()
                .filter(|(_, value)| value.is_none())
                .map(|(key, _)| key.to_str().unwrap())
                .collect();
            for key in PROXY_KEYS {
                assert_eq!(removed.contains(&key), !healthy);
            }
        }
        server.await.unwrap();
        assert!(!probe(&client, "http://health.invalid/").await);
    }
}
