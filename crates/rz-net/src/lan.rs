//! Which addresses count as "LAN" (plan 6.8): RFC 1918, link-local, loopback, CGNAT 100.64/10 (Tailscale),
//! IPv6 ULA and link-local.

use std::net::IpAddr;

pub fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            v.is_private() || v.is_link_local() || v.is_loopback() || (o[0] == 100 && (o[1] & 0xC0) == 64)
        }
        IpAddr::V6(v) => {
            let s = v.segments()[0];
            v.is_loopback() || (s & 0xfe00) == 0xfc00 || (s & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        for ok in [
            "192.168.88.32",
            "10.0.0.1",
            "172.20.10.3",
            "169.254.1.1",
            "127.0.0.1",
            "100.73.177.99",
            "fd00::1",
            "fe80::1",
        ] {
            assert!(is_lan(ok.parse().unwrap()), "{ok}");
        }
        for no in ["8.8.8.8", "100.128.0.1", "172.32.0.1", "2001:db8::1"] {
            assert!(!is_lan(no.parse().unwrap()), "{no}");
        }
    }
}
