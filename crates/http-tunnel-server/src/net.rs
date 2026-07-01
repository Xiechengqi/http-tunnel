use axum::http::HeaderMap;
use std::net::{IpAddr, SocketAddr};

pub fn client_ip(
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    trust_proxy_headers: bool,
    trusted_proxy_cidrs: &[String],
) -> String {
    client_ip_from_headers(
        headers,
        remote_addr,
        trust_proxy_headers,
        trusted_proxy_cidrs,
    )
    .unwrap_or_else(|| remote_addr.ip().to_string())
}

pub fn client_ip_from_headers(
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    trust_proxy_headers: bool,
    trusted_proxy_cidrs: &[String],
) -> Option<String> {
    if !trust_proxy_headers || !proxy_is_trusted(remote_addr.ip(), trusted_proxy_cidrs) {
        return None;
    }
    headers
        .get("cf-connecting-ip")
        .and_then(|value| value.to_str().ok())
        .and_then(normalize_header_ip)
        .or_else(|| {
            headers
                .get("x-forwarded-for")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(',').next())
                .and_then(normalize_header_ip)
        })
}

pub fn client_country_code_from_headers(
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    trust_proxy_headers: bool,
    trusted_proxy_cidrs: &[String],
) -> Option<String> {
    if !trust_proxy_headers || !proxy_is_trusted(remote_addr.ip(), trusted_proxy_cidrs) {
        return None;
    }
    headers
        .get("cf-ipcountry")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| crate::geoip::normalize_country_code(Some(value)))
}

fn normalize_header_ip(value: &str) -> Option<String> {
    value
        .trim()
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .ok()
        .map(|ip| ip.to_string())
}

pub fn cidr_is_valid(cidr: &str) -> bool {
    parse_cidr(cidr).is_some()
}

pub fn proxy_is_trusted(ip: IpAddr, trusted_proxy_cidrs: &[String]) -> bool {
    trusted_proxy_cidrs.iter().any(|cidr| ip_in_cidr(ip, cidr))
}

fn ip_in_cidr(ip: IpAddr, cidr: &str) -> bool {
    let Some((network, prefix)) = parse_cidr(cidr) else {
        return false;
    };
    match (ip, network) {
        (IpAddr::V4(ip), IpAddr::V4(network)) => {
            let prefix = prefix.min(32);
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            u32::from(ip) & mask == u32::from(network) & mask
        }
        (IpAddr::V6(ip), IpAddr::V6(network)) => {
            let prefix = prefix.min(128);
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            u128::from(ip) & mask == u128::from(network) & mask
        }
        _ => false,
    }
}

fn parse_cidr(cidr: &str) -> Option<(IpAddr, u32)> {
    let (ip, prefix) = cidr.split_once('/').unwrap_or((cidr, ""));
    let ip = ip.parse::<IpAddr>().ok()?;
    let max_prefix = match ip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    let prefix = if prefix.is_empty() {
        max_prefix
    } else {
        prefix.parse::<u32>().ok()?
    };
    if prefix <= max_prefix {
        Some((ip, prefix))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_cidr_syntax() {
        assert!(cidr_is_valid("127.0.0.1/32"));
        assert!(cidr_is_valid("10.0.0.0/8"));
        assert!(cidr_is_valid("::1/128"));
        assert!(!cidr_is_valid("127.0.0.1/33"));
        assert!(!cidr_is_valid("bad"));
    }

    #[test]
    fn trusts_forwarded_for_only_from_trusted_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.10".parse().unwrap());
        let trusted = vec!["127.0.0.1/32".to_string()];
        let untrusted = vec!["10.0.0.0/8".to_string()];
        let remote = "127.0.0.1:1234".parse().unwrap();

        assert_eq!(client_ip(&headers, remote, true, &trusted), "198.51.100.10");
        assert_eq!(client_ip(&headers, remote, true, &untrusted), "127.0.0.1");
        assert_eq!(client_ip(&headers, remote, false, &trusted), "127.0.0.1");
    }

    #[test]
    fn cloudflare_headers_take_precedence_when_proxy_is_trusted() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", "203.0.113.7".parse().unwrap());
        headers.insert("x-forwarded-for", "198.51.100.10".parse().unwrap());
        headers.insert("cf-ipcountry", "us".parse().unwrap());
        let trusted = vec!["173.245.48.0/20".to_string()];
        let remote = "173.245.48.10:443".parse().unwrap();

        assert_eq!(client_ip(&headers, remote, true, &trusted), "203.0.113.7");
        assert_eq!(
            client_country_code_from_headers(&headers, remote, true, &trusted),
            Some("US".to_string())
        );
    }

    #[test]
    fn cloudflare_headers_are_ignored_from_untrusted_peer() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", "203.0.113.7".parse().unwrap());
        headers.insert("cf-ipcountry", "us".parse().unwrap());
        let trusted = vec!["173.245.48.0/20".to_string()];
        let remote = "198.51.100.20:443".parse().unwrap();

        assert_eq!(client_ip(&headers, remote, true, &trusted), "198.51.100.20");
        assert_eq!(
            client_country_code_from_headers(&headers, remote, true, &trusted),
            None
        );
    }

    #[test]
    fn malformed_proxy_ip_headers_are_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", "not-an-ip".parse().unwrap());
        headers.insert("x-forwarded-for", "also-bad".parse().unwrap());
        let trusted = vec!["173.245.48.0/20".to_string()];
        let remote = "173.245.48.10:443".parse().unwrap();

        assert_eq!(client_ip(&headers, remote, true, &trusted), "173.245.48.10");
    }
}
