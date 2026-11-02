//! "I am here": the DNS-SD announcement (`_hesewake-media._tcp`) that the phone looks for on the local network. It
//! carries the name of this PC, the port and `v=1`, and nothing else: no secret and nothing about what plays.

use mdns_sd::{ServiceDaemon, ServiceInfo};

pub const SERVICE: &str = "_hesewake-media._tcp.local.";

pub struct Advert {
    daemon: ServiceDaemon,
    fullname: String,
}

/// A name the phone can show: letters, digits, spaces and a few marks, at most 40 characters.
pub fn instance_name(computer: &str) -> String {
    let cleaned: String = computer.chars().filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.')).take(40).collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        "Hesewake Notch".to_string()
    } else {
        cleaned
    }
}

/// A host name for the A record: letters, digits and hyphens.
fn host_label(computer: &str) -> String {
    let h: String = computer.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let h = h.trim_matches('-').to_string();
    if h.is_empty() {
        "hesewake-pc".to_string()
    } else {
        h
    }
}

impl Advert {
    /// Announce on the network. `addrs` are the addresses to tell (see `net::addresses`); with none, every address of this PC is told,
    /// which is better than not being found at all.
    pub fn start(computer: &str, port: u16, addrs: &[std::net::Ipv4Addr]) -> Result<Advert, String> {
        let daemon = ServiceDaemon::new().map_err(|e| e.to_string())?;
        // not on a tunnel or a virtual adapter: a phone cannot be there, and the answer would only point it the wrong way
        for name in crate::net::virtual_adapters() {
            let _ = daemon.disable_interface(name.as_str());
        }
        let instance = instance_name(computer);
        let host = format!("{}.local.", host_label(computer));
        let ips = addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(",");
        let mut info = ServiceInfo::new(SERVICE, &instance, &host, ips.as_str(), port, &[("v", "1")][..]).map_err(|e| e.to_string())?;
        if addrs.is_empty() {
            info = info.enable_addr_auto();
        }
        let fullname = info.get_fullname().to_string();
        daemon.register(info).map_err(|e| e.to_string())?;
        Ok(Advert { daemon, fullname })
    }
}

impl Drop for Advert {
    fn drop(&mut self) {
        // tell the network we are gone, then stop
        if let Ok(rx) = self.daemon.unregister(&self.fullname) {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(500));
        }
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdns_sd::ServiceEvent;
    use std::time::{Duration, Instant};

    #[test]
    fn names_are_cleaned_for_the_network() {
        assert_eq!(instance_name("THINKPAD"), "THINKPAD");
        assert_eq!(instance_name("  My PC (work)  "), "My PC work");
        assert_eq!(instance_name(""), "Hesewake Notch");
        assert_eq!(instance_name("\u{7}\u{0}"), "Hesewake Notch");
        assert_eq!(instance_name(&"a".repeat(100)).len(), 40);
        assert_eq!(host_label("My PC_1"), "My-PC-1");
        assert_eq!(host_label("---"), "hesewake-pc");
    }

    /// Announce, and find the announcement the way the phone does (this needs multicast on this PC's network).
    #[test]
    #[ignore = "needs multicast on the local network"]
    fn the_announcement_can_be_found() {
        let name = format!("BridgeTest{}", std::process::id());
        let ad = Advert::start(&name, 45678, &crate::net::addresses()).expect("announce");
        let browser = ServiceDaemon::new().unwrap();
        let rx = browser.browse(SERVICE).unwrap();
        let until = Instant::now() + Duration::from_secs(8);
        let mut found = None;
        while Instant::now() < until && found.is_none() {
            if let Ok(ServiceEvent::ServiceResolved(info)) = rx.recv_timeout(Duration::from_millis(300)) {
                if info.get_fullname().starts_with(&name) {
                    found = Some(info);
                }
            }
        }
        let info = found.expect("the service was resolved");
        assert_eq!(info.get_port(), 45678);
        assert_eq!(info.get_property_val_str("v"), Some("1"));
        assert!(!info.get_addresses().is_empty());
        drop(ad);
        let _ = browser.shutdown();
    }
}
