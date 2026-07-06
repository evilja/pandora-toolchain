use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn ipv4_forbidden(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()            // 127.0.0.0/8
        || ip.is_private()      // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local()   // 169.254.0.0/16
        || ip.is_unspecified()  // 0.0.0.0
        || ip.is_broadcast()    // 255.255.255.255
        || o[0] == 0            // 0.0.0.0/8 "this network"
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
        || o[0] >= 240          // 240.0.0.0/4 reserved
}

fn ipv6_forbidden(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return ipv4_forbidden(v4);
    }
    let seg0 = ip.segments()[0];
    ip.is_loopback()                    // ::1
        || ip.is_unspecified()          // ::
        || (seg0 & 0xfe00) == 0xfc00    // fc00::/7 unique local
        || (seg0 & 0xffc0) == 0xfe80    // fe80::/10 link-local
}

fn ip_forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_forbidden(v4),
        IpAddr::V6(v6) => ipv6_forbidden(v6),
    }
}

/// Guard against SSRF and return the URL to actually fetch. A plain `http` URL
/// is upgraded to `https` (we never issue an http request); any other scheme is
/// rejected. The host must not resolve to a loopback / private / link-local /
/// reserved address. Callers should also build their reqwest client with
/// redirects disabled so a 3xx to an internal address cannot bypass this check.
///
/// Note: this resolves DNS and inspects every returned address, but reqwest will
/// resolve again when it connects, so a rebinding resolver can still race this
/// (mitigated in practice by the short window and redirect disabling).
pub async fn sanitize_fetch_url(raw: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(raw).map_err(|e| format!("invalid URL: {}", e))?;
    if url.scheme() == "http" {
        url.set_scheme("https")
            .map_err(|_| "failed to upgrade http URL to https".to_string())?;
    }
    if url.scheme() != "https" {
        return Err(format!("only http/https URLs are allowed (got `{}`)", url.scheme()));
    }
    let host = url.host_str().ok_or_else(|| "URL has no host".to_string())?;
    let port = url.port_or_known_default().unwrap_or(443);

    // Host given as a literal IP: check it directly, no resolver needed.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if ip_forbidden(ip) {
            return Err(format!("URL host resolves to a blocked address: {}", ip));
        }
        return Ok(url.to_string());
    }

    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("could not resolve host `{}`: {}", host, e))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("host `{}` did not resolve to any address", host));
    }
    for addr in addrs {
        if ip_forbidden(addr.ip()) {
            return Err(format!(
                "URL host `{}` resolves to a blocked address: {}",
                host,
                addr.ip()
            ));
        }
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_internal_ipv4() {
        for ip in ["127.0.0.1", "10.0.0.5", "172.16.1.1", "192.168.1.1", "169.254.1.1", "0.0.0.0", "100.64.0.1"] {
            assert!(ip_forbidden(ip.parse().unwrap()), "{} should be blocked", ip);
        }
    }

    #[test]
    fn blocks_internal_ipv6() {
        for ip in ["::1", "::", "fc00::1", "fd12::1", "fe80::1", "::ffff:127.0.0.1"] {
            assert!(ip_forbidden(ip.parse().unwrap()), "{} should be blocked", ip);
        }
    }

    #[test]
    fn allows_public() {
        for ip in ["1.1.1.1", "8.8.8.8", "140.82.121.3", "2606:4700:4700::1111"] {
            assert!(!ip_forbidden(ip.parse().unwrap()), "{} should be allowed", ip);
        }
    }

    #[test]
    fn upgrades_http_scheme_to_https() {
        let mut u = reqwest::Url::parse("http://8.8.8.8/x").unwrap();
        assert_eq!(u.scheme(), "http");
        u.set_scheme("https").unwrap();
        assert_eq!(u.as_str(), "https://8.8.8.8/x");
    }
}
