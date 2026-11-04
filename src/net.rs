//! This PC's addresses, for the QR code and for kind 18: how a paired device can reach it when it
//! can't find it by Bonjour. A VPN's (Tailscale's) come first, since those work away from home.

use crate::protocol::MacAddress;

/// A kind for an address, or None for one no other device could use.
fn classify(ip: std::net::IpAddr) -> Option<&'static str> {
    use std::net::IpAddr::*;
    match ip {
        V4(a) if a.is_loopback() || a.is_link_local() || a.is_unspecified() => None,
        V4(a) if a.octets()[0] == 100 && (64..128).contains(&a.octets()[1]) => Some("vpn"),
        V4(a) if a.is_private() => Some("lan"),
        V4(_) => Some("internet"),
        V6(a) if a.is_loopback() || (a.segments()[0] & 0xffc0) == 0xfe80 || a.is_unspecified() => None,
        V6(a) if a.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0] => Some("vpn"),
        V6(_) => None, // shortcut: ordinary IPv6 left out, as they change; add them if a network needs them
    }
}

fn entry(ip: std::net::IpAddr, adapter: &str) -> Option<MacAddress> {
    let kind = classify(ip)?;
    let via = if kind == "vpn" { "Tailscale" } else if kind == "lan" { adapter } else { "Router" };
    Some(MacAddress { host: ip.to_string(), kind: kind.into(), via: via.into() })
}

/// In dial order, at most five.
pub fn addresses() -> Vec<MacAddress> {
    let mut list: Vec<MacAddress> = platform::interfaces().into_iter().filter_map(|(ip, name)| entry(ip, &name)).collect();
    list.sort_by_key(|a| match a.kind.as_str() {
        "vpn" => 0,
        "lan" => 1,
        _ => 2,
    });
    list.truncate(5);
    list
}

#[cfg(windows)]
mod platform {
    use windows::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
    use windows::Win32::NetworkManagement::IpHelper::*;
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows::Win32::Networking::WinSock::*;

    pub fn interfaces() -> Vec<(std::net::IpAddr, String)> {
        let mut out = Vec::new();
        unsafe {
            let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
            let mut size = 16 * 1024u32;
            let mut buf = vec![0u64; size as usize / 8];
            let mut r = GetAdaptersAddresses(0, flags, None, Some(buf.as_mut_ptr() as *mut _), &mut size);
            if r == ERROR_BUFFER_OVERFLOW.0 {
                buf = vec![0u64; size as usize / 8 + 1];
                r = GetAdaptersAddresses(0, flags, None, Some(buf.as_mut_ptr() as *mut _), &mut size);
            }
            if r != 0 {
                return out;
            }
            let mut a = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
            while !a.is_null() {
                let ad = &*a;
                if ad.OperStatus == IfOperStatusUp {
                    let name = ad.FriendlyName.to_string().unwrap_or_default();
                    let mut u = ad.FirstUnicastAddress;
                    while !u.is_null() {
                        let sa = (*u).Address.lpSockaddr;
                        if !sa.is_null() {
                            match (*sa).sa_family {
                                AF_INET => {
                                    let v4 = &*(sa as *const SOCKADDR_IN);
                                    out.push((std::net::Ipv4Addr::from(v4.sin_addr.S_un.S_addr.to_ne_bytes()).into(), name.clone()));
                                }
                                AF_INET6 => {
                                    let v6 = &*(sa as *const SOCKADDR_IN6);
                                    out.push((std::net::Ipv6Addr::from(v6.sin6_addr.u.Byte).into(), name.clone()));
                                }
                                _ => {}
                            }
                        }
                        u = (*u).Next;
                    }
                }
                a = ad.Next;
            }
        }
        out
    }
}

// Outside Windows (development and tests): the one address the default route uses.
#[cfg(not(windows))]
mod platform {
    pub fn interfaces() -> Vec<(std::net::IpAddr, String)> {
        let found = (|| {
            let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
            s.connect("192.0.2.1:9").ok()?; // nothing is sent: this only picks the interface
            Some(s.local_addr().ok()?.ip())
        })();
        found.into_iter().map(|ip| (ip, "Wi-Fi".to_string())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_addresses_count() {
        let ip = |s: &str| s.parse().unwrap();
        assert_eq!(classify(ip("100.101.102.103")), Some("vpn"));
        assert_eq!(classify(ip("100.128.0.1")), Some("internet"));
        assert_eq!(classify(ip("192.168.1.20")), Some("lan"));
        assert_eq!(classify(ip("127.0.0.1")), None);
        assert_eq!(classify(ip("169.254.3.4")), None);
        assert_eq!(classify(ip("fe80::1")), None);
        assert_eq!(classify(ip("fd7a:115c:a1e0::1234")), Some("vpn"));
        assert_eq!(entry(ip("100.100.1.1"), "Ethernet").unwrap().via, "Tailscale");
        assert_eq!(entry(ip("192.168.1.2"), "Wi-Fi 2").unwrap().via, "Wi-Fi 2");
    }
}
