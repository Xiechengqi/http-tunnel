use axum::http::HeaderMap;
use std::net::{IpAddr, SocketAddr};

pub fn client_ip(
    headers: &HeaderMap,
    remote_addr: SocketAddr,
    trust_proxy_headers: bool,
    trusted_proxy_cidrs: &[String],
) -> String {
    if trust_proxy_headers && proxy_is_trusted(remote_addr.ip(), trusted_proxy_cidrs) {
        if let Some(forwarded) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return forwarded.to_string();
        }
    }
    remote_addr.ip().to_string()
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
}
