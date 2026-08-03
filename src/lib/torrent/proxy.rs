use std::fmt::{Debug, Formatter};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket, lookup_host};
use tokio::time::timeout;

use super::error::{Result, TorrentError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyKind {
    Http,
    Socks5,
}

#[derive(Clone)]
pub struct ProxyConfig {
    kind: ProxyKind,
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
    remote_dns: bool,
    original: String,
}

impl Debug for ProxyConfig {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProxyConfig")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("authenticated", &self.username.is_some())
            .field("remote_dns", &self.remote_dns)
            .finish()
    }
}

impl ProxyConfig {
    pub fn parse(value: &str) -> Result<Self> {
        let url =
            Url::parse(value).map_err(|error| TorrentError::InvalidProxy(error.to_string()))?;
        let (kind, remote_dns, default_port) = match url.scheme() {
            "http" => (ProxyKind::Http, false, 80),
            "socks5" => (ProxyKind::Socks5, false, 1080),
            "socks5h" => (ProxyKind::Socks5, true, 1080),
            scheme => {
                return Err(TorrentError::InvalidProxy(format!(
                    "unsupported proxy scheme {scheme}; use http, socks5, or socks5h"
                )));
            }
        };
        let host = url
            .host_str()
            .filter(|host| !host.is_empty())
            .ok_or_else(|| TorrentError::InvalidProxy("proxy host is missing".to_string()))?
            .to_string();
        let port = url.port().unwrap_or(default_port);
        let username = if url.username().is_empty() {
            None
        } else {
            Some(percent_decode(url.username())?)
        };
        let password = url.password().map(percent_decode).transpose()?;
        if username.is_none() && password.is_some() {
            return Err(TorrentError::InvalidProxy(
                "proxy password requires a username".to_string(),
            ));
        }
        Ok(Self {
            kind,
            host,
            port,
            username,
            password,
            remote_dns,
            original: value.to_string(),
        })
    }

    pub fn from_env() -> Result<Option<Self>> {
        for key in ["PNP2P_PROXY", "ALL_PROXY", "all_proxy"] {
            if let Ok(value) = std::env::var(key) {
                if !value.trim().is_empty() {
                    return Self::parse(value.trim()).map(Some);
                }
            }
        }
        Ok(None)
    }

    pub fn kind(&self) -> ProxyKind {
        self.kind
    }

    pub(crate) fn reqwest_url(&self) -> &str {
        &self.original
    }

    async fn connect_control(&self, timeout_duration: Duration) -> Result<TcpStream> {
        let addresses = lookup_host((self.host.as_str(), self.port)).await?;
        let mut last_error = None;
        for address in addresses {
            match timeout(timeout_duration, TcpStream::connect(address)).await {
                Ok(Ok(stream)) => {
                    stream.set_nodelay(true)?;
                    return Ok(stream);
                }
                Ok(Err(error)) => last_error = Some(error),
                Err(_) => {}
            }
        }
        Err(last_error
            .map(TorrentError::Io)
            .unwrap_or(TorrentError::Timeout("connecting to the proxy")))
    }
}

pub(crate) async fn connect_peer(
    target: SocketAddr,
    proxy: Option<&ProxyConfig>,
    timeout_duration: Duration,
) -> Result<TcpStream> {
    let Some(proxy) = proxy else {
        let stream = timeout(timeout_duration, TcpStream::connect(target))
            .await
            .map_err(|_| TorrentError::Timeout("connecting to a peer"))??;
        stream.set_nodelay(true)?;
        return Ok(stream);
    };
    let mut stream = proxy.connect_control(timeout_duration).await?;
    match proxy.kind {
        ProxyKind::Http => http_connect(&mut stream, target, proxy, timeout_duration).await?,
        ProxyKind::Socks5 => {
            socks_authenticate(&mut stream, proxy, timeout_duration).await?;
            let request = socks_request(0x01, SocksTarget::Ip(target))?;
            timed_write_all(&mut stream, &request, timeout_duration).await?;
            read_socks_reply(&mut stream, timeout_duration).await?;
        }
    }
    Ok(stream)
}

pub(crate) struct SocksUdp {
    _control: TcpStream,
    socket: UdpSocket,
    relay: SocketAddr,
    timeout: Duration,
}

impl SocksUdp {
    pub(crate) async fn associate(proxy: &ProxyConfig, timeout_duration: Duration) -> Result<Self> {
        if proxy.kind != ProxyKind::Socks5 {
            return Err(TorrentError::Unsupported(
                "UDP trackers cannot use an HTTP CONNECT proxy".to_string(),
            ));
        }
        let mut control = proxy.connect_control(timeout_duration).await?;
        let proxy_ip = control.peer_addr()?.ip();
        socks_authenticate(&mut control, proxy, timeout_duration).await?;
        let bind = match proxy_ip {
            IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0),
        };
        let request = socks_request(0x03, SocksTarget::Ip(bind))?;
        timed_write_all(&mut control, &request, timeout_duration).await?;
        let mut relay = read_socks_reply(&mut control, timeout_duration).await?;
        if relay.ip().is_unspecified() {
            relay.set_ip(proxy_ip);
        }
        let socket = UdpSocket::bind(bind).await?;
        Ok(Self {
            _control: control,
            socket,
            relay,
            timeout: timeout_duration,
        })
    }

    pub(crate) async fn exchange(&self, host: &str, port: u16, payload: &[u8]) -> Result<Vec<u8>> {
        let target = host
            .parse::<IpAddr>()
            .map(|address| SocksTarget::Ip(SocketAddr::new(address, port)))
            .unwrap_or(SocksTarget::Domain(host, port));
        let mut packet = vec![0, 0, 0];
        encode_socks_target(target, &mut packet)?;
        packet.extend_from_slice(payload);
        timeout(self.timeout, self.socket.send_to(&packet, self.relay))
            .await
            .map_err(|_| TorrentError::Timeout("sending a proxied UDP tracker request"))??;
        let mut response = vec![0u8; 65_535];
        let (length, _) = timeout(self.timeout, self.socket.recv_from(&mut response))
            .await
            .map_err(|_| TorrentError::Timeout("reading a proxied UDP tracker response"))??;
        response.truncate(length);
        decode_socks_udp_packet(&response)
    }
}

async fn http_connect(
    stream: &mut TcpStream,
    target: SocketAddr,
    proxy: &ProxyConfig,
    timeout_duration: Duration,
) -> Result<()> {
    let authority = match target {
        SocketAddr::V4(value) => value.to_string(),
        SocketAddr::V6(value) => format!("[{}]:{}", value.ip(), value.port()),
    };
    let mut request = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: Keep-Alive\r\n"
    );
    if let Some(username) = proxy.username.as_ref() {
        let credentials = format!("{}:{}", username, proxy.password.as_deref().unwrap_or(""));
        request.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64(credentials.as_bytes())
        ));
    }
    request.push_str("\r\n");
    timed_write_all(stream, request.as_bytes(), timeout_duration).await?;

    let mut response = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        if response.len() >= 16 * 1024 {
            return Err(TorrentError::InvalidProxy(
                "HTTP proxy response headers are too large".to_string(),
            ));
        }
        timed_read_exact(stream, &mut byte, timeout_duration).await?;
        response.push(byte[0]);
    }
    let status = String::from_utf8_lossy(&response)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| {
            TorrentError::InvalidProxy("HTTP proxy returned an invalid response".to_string())
        })?;
    if !(200..300).contains(&status) {
        return Err(TorrentError::InvalidProxy(format!(
            "HTTP CONNECT proxy returned status {status}"
        )));
    }
    Ok(())
}

async fn socks_authenticate(
    stream: &mut TcpStream,
    proxy: &ProxyConfig,
    timeout_duration: Duration,
) -> Result<()> {
    let methods: &[u8] = if proxy.username.is_some() {
        &[0x05, 0x02, 0x00, 0x02]
    } else {
        &[0x05, 0x01, 0x00]
    };
    timed_write_all(stream, methods, timeout_duration).await?;
    let mut response = [0u8; 2];
    timed_read_exact(stream, &mut response, timeout_duration).await?;
    if response[0] != 0x05 {
        return Err(TorrentError::InvalidProxy(
            "SOCKS proxy returned an invalid version".to_string(),
        ));
    }
    match response[1] {
        0x00 => Ok(()),
        0x02 => {
            let username = proxy.username.as_deref().unwrap_or("").as_bytes();
            let password = proxy.password.as_deref().unwrap_or("").as_bytes();
            if username.len() > 255 || password.len() > 255 {
                return Err(TorrentError::InvalidProxy(
                    "SOCKS username or password exceeds 255 bytes".to_string(),
                ));
            }
            let mut request = Vec::with_capacity(3 + username.len() + password.len());
            request.extend_from_slice(&[0x01, username.len() as u8]);
            request.extend_from_slice(username);
            request.push(password.len() as u8);
            request.extend_from_slice(password);
            timed_write_all(stream, &request, timeout_duration).await?;
            timed_read_exact(stream, &mut response, timeout_duration).await?;
            if response != [0x01, 0x00] {
                return Err(TorrentError::InvalidProxy(
                    "SOCKS username/password authentication failed".to_string(),
                ));
            }
            Ok(())
        }
        0xff => Err(TorrentError::InvalidProxy(
            "SOCKS proxy rejected all authentication methods".to_string(),
        )),
        method => Err(TorrentError::InvalidProxy(format!(
            "SOCKS proxy selected unsupported authentication method {method}"
        ))),
    }
}

#[derive(Clone, Copy)]
enum SocksTarget<'a> {
    Ip(SocketAddr),
    Domain(&'a str, u16),
}

fn socks_request(command: u8, target: SocksTarget<'_>) -> Result<Vec<u8>> {
    let mut request = vec![0x05, command, 0x00];
    encode_socks_target(target, &mut request)?;
    Ok(request)
}

fn encode_socks_target(target: SocksTarget<'_>, output: &mut Vec<u8>) -> Result<()> {
    match target {
        SocksTarget::Ip(SocketAddr::V4(address)) => {
            output.push(0x01);
            output.extend_from_slice(&address.ip().octets());
            output.extend_from_slice(&address.port().to_be_bytes());
        }
        SocksTarget::Ip(SocketAddr::V6(address)) => {
            output.push(0x04);
            output.extend_from_slice(&address.ip().octets());
            output.extend_from_slice(&address.port().to_be_bytes());
        }
        SocksTarget::Domain(host, port) => {
            if host.is_empty() || host.len() > 255 {
                return Err(TorrentError::InvalidProxy(
                    "SOCKS target hostname has an invalid length".to_string(),
                ));
            }
            output.extend_from_slice(&[0x03, host.len() as u8]);
            output.extend_from_slice(host.as_bytes());
            output.extend_from_slice(&port.to_be_bytes());
        }
    }
    Ok(())
}

async fn read_socks_reply(
    stream: &mut TcpStream,
    timeout_duration: Duration,
) -> Result<SocketAddr> {
    let mut header = [0u8; 4];
    timed_read_exact(stream, &mut header, timeout_duration).await?;
    if header[0] != 0x05 || header[2] != 0x00 {
        return Err(TorrentError::InvalidProxy(
            "SOCKS proxy returned an invalid reply".to_string(),
        ));
    }
    if header[1] != 0x00 {
        return Err(TorrentError::InvalidProxy(format!(
            "SOCKS proxy request failed with status {}",
            header[1]
        )));
    }
    let ip = match header[3] {
        0x01 => {
            let mut bytes = [0u8; 4];
            timed_read_exact(stream, &mut bytes, timeout_duration).await?;
            IpAddr::V4(Ipv4Addr::from(bytes))
        }
        0x04 => {
            let mut bytes = [0u8; 16];
            timed_read_exact(stream, &mut bytes, timeout_duration).await?;
            IpAddr::V6(std::net::Ipv6Addr::from(bytes))
        }
        0x03 => {
            let mut length = [0u8; 1];
            timed_read_exact(stream, &mut length, timeout_duration).await?;
            let mut domain = vec![0u8; usize::from(length[0])];
            timed_read_exact(stream, &mut domain, timeout_duration).await?;
            let domain = String::from_utf8(domain).map_err(|_| {
                TorrentError::InvalidProxy("SOCKS reply hostname is not UTF-8".to_string())
            })?;
            lookup_host((domain.as_str(), 0))
                .await?
                .next()
                .map(|address| address.ip())
                .ok_or_else(|| {
                    TorrentError::InvalidProxy("SOCKS reply hostname did not resolve".to_string())
                })?
        }
        atyp => {
            return Err(TorrentError::InvalidProxy(format!(
                "SOCKS proxy returned unknown address type {atyp}"
            )));
        }
    };
    let mut port = [0u8; 2];
    timed_read_exact(stream, &mut port, timeout_duration).await?;
    Ok(SocketAddr::new(ip, u16::from_be_bytes(port)))
}

fn decode_socks_udp_packet(packet: &[u8]) -> Result<Vec<u8>> {
    if packet.len() < 4 || packet[0..2] != [0, 0] || packet[2] != 0 {
        return Err(TorrentError::InvalidProxy(
            "invalid or fragmented SOCKS UDP response".to_string(),
        ));
    }
    let mut position = 3usize;
    match packet[position] {
        0x01 => position += 1 + 4,
        0x04 => position += 1 + 16,
        0x03 => {
            let length = usize::from(*packet.get(position + 1).ok_or_else(|| {
                TorrentError::InvalidProxy("truncated SOCKS UDP response".to_string())
            })?);
            position += 2 + length;
        }
        _ => {
            return Err(TorrentError::InvalidProxy(
                "unknown SOCKS UDP address type".to_string(),
            ));
        }
    }
    position = position
        .checked_add(2)
        .ok_or_else(|| TorrentError::InvalidProxy("SOCKS UDP offset overflow".to_string()))?;
    packet
        .get(position..)
        .map(ToOwned::to_owned)
        .ok_or_else(|| TorrentError::InvalidProxy("truncated SOCKS UDP response".to_string()))
}

async fn timed_write_all(
    stream: &mut TcpStream,
    data: &[u8],
    timeout_duration: Duration,
) -> Result<()> {
    timeout(timeout_duration, stream.write_all(data))
        .await
        .map_err(|_| TorrentError::Timeout("writing to the proxy"))??;
    Ok(())
}

async fn timed_read_exact(
    stream: &mut TcpStream,
    data: &mut [u8],
    timeout_duration: Duration,
) -> Result<()> {
    timeout(timeout_duration, stream.read_exact(data))
        .await
        .map_err(|_| TorrentError::Timeout("reading from the proxy"))??;
    Ok(())
}

fn percent_decode(value: &str) -> Result<String> {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = bytes.get(index + 1..index + 3).ok_or_else(|| {
                TorrentError::InvalidProxy("truncated percent escape in credentials".to_string())
            })?;
            let high = hex(pair[0])?;
            let low = hex(pair[1])?;
            output.push((high << 4) | low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output)
        .map_err(|_| TorrentError::InvalidProxy("proxy credentials are not UTF-8".to_string()))
}

fn hex(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(TorrentError::InvalidProxy(
            "invalid percent escape in proxy credentials".to_string(),
        )),
    }
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(ALPHABET[((value >> 18) & 63) as usize] as char);
        output.push(ALPHABET[((value >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn parses_proxy_urls_without_exposing_passwords() {
        let proxy = ProxyConfig::parse("socks5h://user:p%40ss@example.invalid:1081").unwrap();
        assert_eq!(proxy.kind(), ProxyKind::Socks5);
        assert!(proxy.remote_dns);
        assert_eq!(proxy.username.as_deref(), Some("user"));
        assert_eq!(proxy.password.as_deref(), Some("p@ss"));
        assert!(!format!("{proxy:?}").contains("p@ss"));
    }

    #[test]
    fn encodes_basic_auth() {
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn strips_socks_udp_header() {
        let packet = [0, 0, 0, 1, 127, 0, 0, 1, 0, 80, 1, 2, 3];
        assert_eq!(decode_socks_udp_packet(&packet).unwrap(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn establishes_http_connect_tunnels() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            assert!(String::from_utf8_lossy(&request).starts_with("CONNECT 127.0.0.1:6881 "));
            stream
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            stream.read_exact(&mut byte).await.unwrap();
            assert_eq!(byte[0], 42);
        });
        let proxy = ProxyConfig::parse(&format!("http://{address}")).unwrap();
        let mut tunnel = connect_peer(
            "127.0.0.1:6881".parse().unwrap(),
            Some(&proxy),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        tunnel.write_all(&[42]).await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn establishes_socks5_tunnels() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            stream.write_all(&[5, 0]).await.unwrap();
            let mut request = [0u8; 10];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(&request[..4], &[5, 1, 0, 1]);
            assert_eq!(&request[4..8], &[127, 0, 0, 1]);
            assert_eq!(u16::from_be_bytes(request[8..10].try_into().unwrap()), 6881);
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1])
                .await
                .unwrap();
        });
        let proxy = ProxyConfig::parse(&format!("socks5://{address}")).unwrap();
        connect_peer(
            "127.0.0.1:6881".parse().unwrap(),
            Some(&proxy),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        server.await.unwrap();
    }
}
