//! Which of this PC's addresses a phone can use. A PC often has more than one: the Wi-Fi, and also tunnels and virtual adapters
//! (a VPN such as NetBird or WireGuard, WSL, a virtual machine). A phone on the Wi-Fi cannot reach the others, and if the bridge
//! tells it one of those first, pairing fails: the connection simply never comes. So only addresses of real network adapters in
//! the private ranges are told to the phone, in the QR code and in the announcement.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

/// Adapters that are tunnels or virtual: a word in the name gives them away (a number after it, as in "wt0" or "tun1", does not count).
const VIRTUAL_WORDS: [&str; 18] = [
    "wt", "wg", "netbird", "wireguard", "wintun", "tailscale", "zerotier", "vpn", "vethernet", "vmware", "virtualbox", "vbox", "docker", "wsl", "hyper", "bluetooth", "tap", "tun",
];

/// True for the name of an adapter that is a tunnel or a virtual one, whose addresses a phone on the Wi-Fi cannot reach.
pub fn is_virtual(name: &str) -> bool {
    let lower = name.to_lowercase();
    // the hotspot of Windows ("Local Area Connection* 10") is a virtual adapter that has a private address too
    if lower.starts_with("local area connection*") {
        return true;
    }
    lower.split(|c: char| !c.is_alphanumeric()).any(|w| VIRTUAL_WORDS.contains(&w.trim_end_matches(|c: char| c.is_ascii_digit())))
}

/// The addresses of real adapters in the private ranges (10/8, 172.16/12, 192.168/16), one of each. Not 100.64/10 (what NetBird and
/// Tailscale use), not link-local, not loopback.
pub fn addresses() -> Vec<Ipv4Addr> {
    let all = if_addrs::get_if_addrs().unwrap_or_default();
    let mut out: Vec<Ipv4Addr> = Vec::new();
    for i in all {
        if let IpAddr::V4(a) = i.ip() {
            if !i.is_loopback() && a.is_private() && !is_virtual(&i.name) && !out.contains(&a) {
                out.push(a);
            }
        }
    }
    out
}

/// The names of the virtual adapters on this PC, so that the announcement is not sent on them.
pub fn virtual_adapters() -> Vec<String> {
    let mut v: Vec<String> = if_addrs::get_if_addrs().unwrap_or_default().into_iter().filter(|i| is_virtual(&i.name)).map(|i| i.name).collect();
    v.sort();
    v.dedup();
    v
}

/// The address the system would start from to go out: the one of the route to the internet. Nothing is sent; asking the system which
/// address a route would start from is enough. With a tunnel that takes the default route this can be the tunnel's address.
fn routed() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(a) if !a.is_loopback() && !a.is_unspecified() => Some(a),
        _ => None,
    }
}

/// The one to put in the QR code: the routed address if it is one of the candidates, else the first candidate; None if there are none.
pub fn choose(candidates: &[Ipv4Addr], routed: Option<Ipv4Addr>) -> Option<Ipv4Addr> {
    routed.filter(|r| candidates.contains(r)).or_else(|| candidates.first().copied())
}

/// This PC's address for the phone (see `choose`). If no real adapter has a private address, the routed one, as a last resort.
pub fn primary() -> Option<Ipv4Addr> {
    let r = routed();
    choose(&addresses(), r).or(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnels_and_virtual_adapters_are_known_by_name() {
        for v in ["wt0", "NetBird", "WireGuard Tunnel", "wg0", "Tailscale", "ZeroTier One", "vEthernet (WSL)", "vEthernet (Default Switch)", "VMware Network Adapter VMnet8", "VirtualBox Host-Only Network", "OpenVPN TAP-Windows6", "Local Area Connection* 10", "Bluetooth Network Connection", "tun0", "docker0"] {
            assert!(is_virtual(v), "{v} is virtual");
        }
        for r in ["Wi-Fi", "Wi-Fi 2", "Ethernet", "Ethernet 3", "Local Area Connection", "WLAN", "en0", "Realtek PCIe GbE Family Controller", "Laptop LAN", "Intel(R) Wi-Fi 6 AX201"] {
            assert!(!is_virtual(r), "{r} is real");
        }
    }

    #[test]
    fn the_address_for_the_phone_is_a_real_one_even_when_a_tunnel_takes_the_route() {
        let wifi = Ipv4Addr::new(192, 168, 10, 2);
        let lan = Ipv4Addr::new(10, 0, 0, 7);
        let tunnel = Ipv4Addr::new(100, 110, 95, 87);
        assert_eq!(choose(&[wifi], Some(wifi)), Some(wifi), "the usual case");
        assert_eq!(choose(&[wifi], Some(tunnel)), Some(wifi), "the route goes through the tunnel: not its address");
        assert_eq!(choose(&[wifi, lan], Some(lan)), Some(lan), "of two real ones, the one the system routes by");
        assert_eq!(choose(&[wifi, lan], None), Some(wifi));
        assert_eq!(choose(&[], Some(tunnel)), None, "nothing real: the caller decides what to fall back on");
    }

    #[test]
    fn the_addresses_of_this_pc_are_private_and_not_a_tunnels() {
        for a in addresses() {
            assert!(a.is_private() && !a.is_loopback(), "{a}");
            assert!(!(a.octets()[0] == 100 && (64..128).contains(&a.octets()[1])), "{a} is a carrier-grade address (a tunnel's)");
        }
    }
}
