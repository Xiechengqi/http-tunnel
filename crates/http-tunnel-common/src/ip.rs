use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub fn parse_public_ip(value: &str) -> Option<IpAddr> {
    value.trim().parse::<IpAddr>().ok().filter(is_public_ip)
}

pub fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();
    if ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || octets[0] == 0
        || octets[0] >= 240
    {
        return false;
    }

    if octets[0] == 100 && (64..=127).contains(&octets[1]) {
        return false;
    }
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 0 {
        return false;
    }
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 2 {
        return false;
    }
    if octets[0] == 192 && octets[1] == 88 && octets[2] == 99 {
        return false;
    }
    if octets[0] == 198 && (octets[1] == 18 || octets[1] == 19) {
        return false;
    }
    if octets[0] == 198 && octets[1] == 51 && octets[2] == 100 {
        return false;
    }
    if octets[0] == 203 && octets[1] == 0 && octets[2] == 113 {
        return false;
    }

    true
}

fn is_public_ipv6(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return false;
    }
    if (segments[0] & 0xfe00) == 0xfc00 {
        return false;
    }
    if (segments[0] & 0xffc0) == 0xfe80 {
        return false;
    }
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return false;
    }
    if segments[0] == 0x0100 && segments[1] == 0 {
        return false;
    }
    if ip.to_ipv4_mapped().is_some() {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_addresses() {
        assert!(parse_public_ip("8.8.8.8").is_some());
        assert!(parse_public_ip("1.1.1.1").is_some());
        assert!(parse_public_ip("2606:4700:4700::1111").is_some());
    }

    #[test]
    fn rejects_non_public_addresses() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "169.254.1.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert_eq!(parse_public_ip(ip), None, "{ip}");
        }
    }
}
