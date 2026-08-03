use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use tokio::net::{UdpSocket, lookup_host};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, sleep_until, timeout};

use super::bencode::{Value, decode};
use super::error::{Result, TorrentError};
use super::proxy::{ProxyConfig, ProxyKind, SocksUdp};

const MAX_TRACKER_RESPONSE: usize = 4 * 1024 * 1024;
const TRACKER_PEER_GRACE: Duration = Duration::from_millis(300);

#[derive(Clone)]
pub(crate) struct TrackerClient {
    http: reqwest::Client,
    proxy: Option<ProxyConfig>,
    timeout: Duration,
    peer_id: [u8; 20],
    port: u16,
}

impl TrackerClient {
    pub(crate) fn new(
        proxy: Option<ProxyConfig>,
        timeout_duration: Duration,
        peer_id: [u8; 20],
        port: u16,
    ) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(timeout_duration)
            .timeout(timeout_duration)
            .user_agent("PandoraTorrent/1.0")
            .no_proxy();
        if let Some(proxy) = proxy.as_ref() {
            let reqwest_proxy = reqwest::Proxy::all(proxy.reqwest_url())
                .map_err(|error| TorrentError::InvalidProxy(error.to_string()))?;
            builder = builder.proxy(reqwest_proxy);
        }
        Ok(Self {
            http: builder.build()?,
            proxy,
            timeout: timeout_duration,
            peer_id,
            port,
        })
    }

    pub(crate) async fn announce_all(
        &self,
        trackers: &[String],
        info_hash: [u8; 20],
        left: u64,
    ) -> Result<Vec<SocketAddr>> {
        if trackers.is_empty() {
            return Err(TorrentError::tracker(
                "no HTTP or UDP trackers are available",
            ));
        }
        let mut tasks = JoinSet::new();
        for tracker in trackers.iter().take(64).cloned() {
            let client = self.clone();
            tasks.spawn(async move { client.announce(&tracker, info_hash, left).await });
        }
        let mut peers = Vec::new();
        let mut seen = HashSet::new();
        let mut errors = Vec::new();
        let mut grace_deadline = None;
        loop {
            if tasks.is_empty() {
                break;
            }
            let result = if let Some(deadline) = grace_deadline {
                tokio::select! {
                    result = tasks.join_next() => result,
                    _ = sleep_until(deadline) => {
                        tasks.abort_all();
                        break;
                    }
                }
            } else {
                tasks.join_next().await
            };
            let Some(result) = result else {
                break;
            };
            match result {
                Ok(Ok(found)) => {
                    for peer in found {
                        if seen.insert(peer) {
                            peers.push(peer);
                        }
                    }
                    if !peers.is_empty() && grace_deadline.is_none() {
                        grace_deadline = Some(Instant::now() + TRACKER_PEER_GRACE);
                    }
                }
                Ok(Err(error)) => errors.push(error.to_string()),
                Err(error) => errors.push(error.to_string()),
            }
        }
        if peers.is_empty() && !errors.is_empty() {
            return Err(TorrentError::tracker(errors.join("; ")));
        }
        Ok(peers)
    }

    async fn announce(
        &self,
        tracker: &str,
        info_hash: [u8; 20],
        left: u64,
    ) -> Result<Vec<SocketAddr>> {
        let url = Url::parse(tracker)
            .map_err(|error| TorrentError::tracker(format!("{tracker}: {error}")))?;
        match url.scheme() {
            "http" | "https" => self.announce_http(url, info_hash, left).await,
            "udp" => self.announce_udp(url, info_hash, left).await,
            scheme => Err(TorrentError::Unsupported(format!(
                "tracker scheme {scheme}"
            ))),
        }
    }

    async fn announce_http(
        &self,
        mut url: Url,
        info_hash: [u8; 20],
        left: u64,
    ) -> Result<Vec<SocketAddr>> {
        url.set_fragment(None);
        let separator = if url.query().is_some_and(|query| !query.is_empty()) {
            '&'
        } else {
            '?'
        };
        let request_url = format!(
            "{}{separator}info_hash={}&peer_id={}&port={}&uploaded=0&downloaded=0&left={left}&compact=1&no_peer_id=1&event=started&numwant=200",
            url.as_str(),
            percent_bytes(&info_hash),
            percent_bytes(&self.peer_id),
            self.port,
        );
        let response = self
            .http
            .get(request_url)
            .send()
            .await?
            .error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_TRACKER_RESPONSE as u64)
        {
            return Err(TorrentError::tracker("HTTP tracker response is too large"));
        }
        let bytes = response.bytes().await?;
        if bytes.len() > MAX_TRACKER_RESPONSE {
            return Err(TorrentError::tracker("HTTP tracker response is too large"));
        }
        parse_tracker_response(&bytes)
    }

    async fn announce_udp(
        &self,
        url: Url,
        info_hash: [u8; 20],
        left: u64,
    ) -> Result<Vec<SocketAddr>> {
        let host = url
            .host_str()
            .ok_or_else(|| TorrentError::tracker("UDP tracker host is missing"))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| TorrentError::tracker("UDP tracker port is missing"))?;

        if self
            .proxy
            .as_ref()
            .is_some_and(|proxy| proxy.kind() == ProxyKind::Http)
        {
            return Err(TorrentError::Unsupported(
                "UDP trackers cannot traverse an HTTP proxy".to_string(),
            ));
        }
        let (transport, ipv6_response) = if let Some(proxy) = self.proxy.as_ref() {
            (
                UdpTransport::Socks(Arc::new(SocksUdp::associate(proxy, self.timeout).await?)),
                host.parse::<Ipv6Addr>().is_ok(),
            )
        } else {
            let address = lookup_host((host, port))
                .await?
                .next()
                .ok_or_else(|| TorrentError::tracker("UDP tracker did not resolve"))?;
            let bind = match address {
                SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
                SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
            };
            (
                UdpTransport::Direct(Arc::new(UdpSocket::bind(bind).await?), address),
                address.is_ipv6(),
            )
        };

        let connect_transaction = random_u32()?;
        let mut connect = Vec::with_capacity(16);
        connect.extend_from_slice(&0x41727101980u64.to_be_bytes());
        connect.extend_from_slice(&0u32.to_be_bytes());
        connect.extend_from_slice(&connect_transaction.to_be_bytes());
        let response = retry_udp(&transport, host, port, &connect, self.timeout).await?;
        if response.len() < 16 {
            return Err(TorrentError::tracker("truncated UDP connect response"));
        }
        validate_udp_action(&response, 0, connect_transaction)?;
        let connection_id = u64::from_be_bytes(response[8..16].try_into().unwrap());

        let transaction = random_u32()?;
        let mut announce = Vec::with_capacity(98);
        announce.extend_from_slice(&connection_id.to_be_bytes());
        announce.extend_from_slice(&1u32.to_be_bytes());
        announce.extend_from_slice(&transaction.to_be_bytes());
        announce.extend_from_slice(&info_hash);
        announce.extend_from_slice(&self.peer_id);
        announce.extend_from_slice(&0u64.to_be_bytes());
        announce.extend_from_slice(&left.to_be_bytes());
        announce.extend_from_slice(&0u64.to_be_bytes());
        announce.extend_from_slice(&2u32.to_be_bytes());
        announce.extend_from_slice(&0u32.to_be_bytes());
        announce.extend_from_slice(&random_u32()?.to_be_bytes());
        announce.extend_from_slice(&(-1i32).to_be_bytes());
        announce.extend_from_slice(&self.port.to_be_bytes());
        let response = retry_udp(&transport, host, port, &announce, self.timeout).await?;
        if response.len() < 20 {
            return Err(TorrentError::tracker("truncated UDP announce response"));
        }
        validate_udp_action(&response, 1, transaction)?;
        if ipv6_response {
            parse_compact_v6(&response[20..])
        } else {
            parse_compact_v4(&response[20..])
        }
    }
}

enum UdpTransport {
    Direct(Arc<UdpSocket>, SocketAddr),
    Socks(Arc<SocksUdp>),
}

impl UdpTransport {
    async fn exchange(
        &self,
        host: &str,
        port: u16,
        payload: &[u8],
        wait: Duration,
    ) -> Result<Vec<u8>> {
        match self {
            Self::Direct(socket, address) => {
                socket.send_to(payload, address).await?;
                let mut response = vec![0u8; 65_535];
                let (length, _) = timeout(wait, socket.recv_from(&mut response))
                    .await
                    .map_err(|_| TorrentError::Timeout("waiting for a UDP tracker"))??;
                response.truncate(length);
                Ok(response)
            }
            Self::Socks(socket) => socket.exchange(host, port, payload).await,
        }
    }
}

async fn retry_udp(
    transport: &UdpTransport,
    host: &str,
    port: u16,
    payload: &[u8],
    timeout_duration: Duration,
) -> Result<Vec<u8>> {
    let attempts = [
        Duration::from_secs(2),
        Duration::from_secs(5),
        timeout_duration,
    ];
    let mut last_error = None;
    for wait in attempts {
        match transport.exchange(host, port, payload, wait).await {
            Ok(response) => return Ok(response),
            Err(error) => last_error = Some(error),
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(last_error.unwrap_or(TorrentError::Timeout("contacting a UDP tracker")))
}

fn validate_udp_action(response: &[u8], expected_action: u32, transaction: u32) -> Result<()> {
    let action = u32::from_be_bytes(response[0..4].try_into().unwrap());
    let returned_transaction = u32::from_be_bytes(response[4..8].try_into().unwrap());
    if returned_transaction != transaction {
        return Err(TorrentError::tracker(
            "UDP tracker transaction id does not match",
        ));
    }
    if action == 3 {
        return Err(TorrentError::tracker(String::from_utf8_lossy(
            &response[8..],
        )));
    }
    if action != expected_action {
        return Err(TorrentError::tracker(format!(
            "UDP tracker returned action {action}, expected {expected_action}"
        )));
    }
    Ok(())
}

fn parse_tracker_response(data: &[u8]) -> Result<Vec<SocketAddr>> {
    let value = decode(data)?;
    if let Some(failure) = value.get(b"failure reason").and_then(Value::as_bytes) {
        return Err(TorrentError::tracker(String::from_utf8_lossy(failure)));
    }
    let mut peers = Vec::new();
    if let Some(value) = value.get(b"peers") {
        if let Some(compact) = value.as_bytes() {
            peers.extend(parse_compact_v4(compact)?);
        } else if let Some(list) = value.as_list() {
            for peer in list {
                let Some(dictionary) = peer.as_dictionary() else {
                    continue;
                };
                let Some(ip) = dictionary
                    .get(b"ip".as_slice())
                    .and_then(Value::as_bytes)
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .and_then(|value| value.parse::<IpAddr>().ok())
                else {
                    continue;
                };
                let Some(port) = dictionary
                    .get(b"port".as_slice())
                    .and_then(Value::as_integer)
                    .and_then(|value| u16::try_from(value).ok())
                else {
                    continue;
                };
                if port != 0 {
                    peers.push(SocketAddr::new(ip, port));
                }
            }
        }
    }
    if let Some(peers6) = value.get(b"peers6").and_then(Value::as_bytes) {
        peers.extend(parse_compact_v6(peers6)?);
    }
    peers.sort_unstable();
    peers.dedup();
    Ok(peers)
}

fn parse_compact_v4(data: &[u8]) -> Result<Vec<SocketAddr>> {
    if data.len() % 6 != 0 {
        return Err(TorrentError::tracker(
            "compact IPv4 peer list has an invalid length",
        ));
    }
    Ok(data
        .chunks_exact(6)
        .filter_map(|peer| {
            let port = u16::from_be_bytes([peer[4], peer[5]]);
            (port != 0).then(|| {
                SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(peer[0], peer[1], peer[2], peer[3])),
                    port,
                )
            })
        })
        .collect())
}

fn parse_compact_v6(data: &[u8]) -> Result<Vec<SocketAddr>> {
    if data.len() % 18 != 0 {
        return Err(TorrentError::tracker(
            "compact IPv6 peer list has an invalid length",
        ));
    }
    Ok(data
        .chunks_exact(18)
        .filter_map(|peer| {
            let port = u16::from_be_bytes([peer[16], peer[17]]);
            let address: [u8; 16] = peer[..16].try_into().unwrap();
            (port != 0).then(|| SocketAddr::new(IpAddr::V6(Ipv6Addr::from(address)), port))
        })
        .collect())
}

fn percent_bytes(data: &[u8]) -> String {
    let mut output = String::with_capacity(data.len() * 3);
    for byte in data {
        use std::fmt::Write;
        write!(&mut output, "%{byte:02X}").ok();
    }
    output
}

fn random_u32() -> Result<u32> {
    let mut bytes = [0u8; 4];
    getrandom::getrandom(&mut bytes).map_err(|error| {
        TorrentError::tracker(format!("random number generation failed: {error}"))
    })?;
    Ok(u32::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn parses_compact_tracker_peers() {
        let response = b"d5:peers12:\x7f\x00\x00\x01\x1a\xe1\x01\x02\x03\x04\x00\x50e";
        let peers = parse_tracker_response(response).unwrap();
        assert_eq!(peers[0], "1.2.3.4:80".parse().unwrap());
        assert_eq!(peers[1], "127.0.0.1:6881".parse().unwrap());
    }

    #[test]
    fn rejects_misaligned_compact_peer_lists() {
        assert!(parse_compact_v4(&[0; 5]).is_err());
        assert!(parse_compact_v6(&[0; 17]).is_err());
    }

    #[test]
    fn percent_encodes_binary_query_values() {
        assert_eq!(percent_bytes(&[0, b'A', 255]), "%00%41%FF");
    }

    #[tokio::test]
    async fn announces_to_udp_trackers() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut request = [0u8; 512];
            let (length, client) = socket.recv_from(&mut request).await.unwrap();
            assert_eq!(length, 16);
            assert_eq!(u32::from_be_bytes(request[8..12].try_into().unwrap()), 0);
            let transaction: [u8; 4] = request[12..16].try_into().unwrap();
            let mut response = Vec::new();
            response.extend_from_slice(&0u32.to_be_bytes());
            response.extend_from_slice(&transaction);
            response.extend_from_slice(&42u64.to_be_bytes());
            socket.send_to(&response, client).await.unwrap();

            let (length, client) = socket.recv_from(&mut request).await.unwrap();
            assert_eq!(length, 98);
            assert_eq!(u64::from_be_bytes(request[..8].try_into().unwrap()), 42);
            assert_eq!(u32::from_be_bytes(request[8..12].try_into().unwrap()), 1);
            let transaction: [u8; 4] = request[12..16].try_into().unwrap();
            let mut response = Vec::new();
            response.extend_from_slice(&1u32.to_be_bytes());
            response.extend_from_slice(&transaction);
            response.extend_from_slice(&60u32.to_be_bytes());
            response.extend_from_slice(&0u32.to_be_bytes());
            response.extend_from_slice(&1u32.to_be_bytes());
            response.extend_from_slice(&[127, 0, 0, 1]);
            response.extend_from_slice(&6881u16.to_be_bytes());
            socket.send_to(&response, client).await.unwrap();
        });
        let client = TrackerClient::new(None, Duration::from_secs(2), [7; 20], 6881).unwrap();
        let peers = client
            .announce(&format!("udp://{address}/announce"), [9; 20], 1)
            .await
            .unwrap();
        assert_eq!(peers, vec!["127.0.0.1:6881".parse().unwrap()]);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn announce_all_does_not_wait_for_a_stalled_tracker_after_finding_peers() {
        let fast = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let slow = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fast_address = fast.local_addr().unwrap();
        let slow_address = slow.local_addr().unwrap();
        let fast_server = tokio::spawn(serve_http_tracker(
            fast,
            Duration::ZERO,
            Some("127.0.0.1:6881".parse().unwrap()),
        ));
        let slow_server = tokio::spawn(serve_http_tracker(
            slow,
            Duration::from_secs(3),
            None,
        ));
        let client = TrackerClient::new(None, Duration::from_secs(5), [7; 20], 6881).unwrap();
        let started = Instant::now();
        let peers = client
            .announce_all(
                &[
                    format!("http://{fast_address}/announce"),
                    format!("http://{slow_address}/announce"),
                ],
                [9; 20],
                1,
            )
            .await
            .unwrap();

        assert_eq!(peers, vec!["127.0.0.1:6881".parse().unwrap()]);
        assert!(started.elapsed() < Duration::from_secs(2));
        fast_server.await.unwrap();
        slow_server.abort();
    }

    async fn serve_http_tracker(
        listener: TcpListener,
        delay: Duration,
        peer: Option<SocketAddr>,
    ) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request).await;
        sleep(delay).await;
        let mut body = b"d5:peers".to_vec();
        match peer {
            Some(SocketAddr::V4(peer)) => {
                body.extend_from_slice(b"6:");
                body.extend_from_slice(&peer.ip().octets());
                body.extend_from_slice(&peer.port().to_be_bytes());
            }
            _ => body.extend_from_slice(b"0:"),
        }
        body.push(b'e');
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
    }
}
