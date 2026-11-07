//! Network source for Linux: /proc/net (route, dev, wireless, if_inet6),
//! sysfs, getifaddrs, and `iw` for Wi-Fi details that the kernel only
//! exposes over nl80211. Parsing is split from file access so it can be
//! tested with fixtures.

use std::ffi::CStr;
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use sidebar_core::Source;
use sidebar_core::source::{LinkKind, NetSample};

use crate::traffic::Traffic;

/// How often `iw` is asked for the Wi-Fi network name and bitrate.
const WIFI_REFRESH: Duration = Duration::from_secs(10);
/// ARPHRD_ETHER, from /sys/class/net/*/type.
const ARPHRD_ETHER: &str = "1";

/// `interface` pins the interface; otherwise the one with the default
/// route is followed as it changes.
pub fn network(interface: Option<String>) -> Box<dyn Source<NetSample>> {
    Box::new(NetSource { pinned: interface, traffic: Traffic::default(), wifi: None })
}

struct NetSource {
    pinned: Option<String>,
    traffic: Traffic,
    /// `iw` details for an interface, and when they were read.
    wifi: Option<(String, WifiLink, Instant)>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct WifiLink {
    ssid: Option<String>,
    frequency_mhz: Option<u32>,
    tx_mbps: Option<f32>,
}

impl NetSource {
    fn wifi_link(&mut self, interface: &str) -> WifiLink {
        let fresh = |(name, _, at): &(String, WifiLink, Instant)| name == interface && at.elapsed() < WIFI_REFRESH;
        if !self.wifi.as_ref().is_some_and(fresh) {
            let output = Command::new("iw").args(["dev", interface, "link"]).output();
            let link = output.ok().map(|o| parse_iw_link(&String::from_utf8_lossy(&o.stdout))).unwrap_or_default();
            self.wifi = Some((interface.to_string(), link, Instant::now()));
        }
        self.wifi.as_ref().map(|(_, link, _)| link.clone()).unwrap_or_default()
    }
}

impl Source<NetSample> for NetSource {
    /// `None` while offline (no default route) or when the interface is
    /// gone.
    fn sample(&mut self) -> Option<NetSample> {
        let route = fs::read_to_string("/proc/net/route").unwrap_or_default();
        let interface = match &self.pinned {
            Some(name) => name.clone(),
            None => default_route(&route)?.0,
        };
        let counters = parse_net_dev(&fs::read_to_string("/proc/net/dev").ok()?, &interface)?;
        let rates = self.traffic.update(&interface, counters);

        let sys = Path::new("/sys/class/net").join(&interface);
        let read = |name: &str| fs::read_to_string(sys.join(name)).map(|s| s.trim().to_string()).ok();
        let kind = if sys.join("wireless").exists() || sys.join("phy80211").exists() {
            LinkKind::Wireless
        } else if read("type").as_deref() == Some(ARPHRD_ETHER) && sys.join("device").exists() {
            // Physical NICs have a device; bridges and veths do not.
            LinkKind::Wired
        } else {
            LinkKind::Other
        };
        let wifi = if kind == LinkKind::Wireless { self.wifi_link(&interface) } else { WifiLink::default() };
        let link_mbps = match kind {
            LinkKind::Wired => read("speed").and_then(|s| s.parse::<i64>().ok()).filter(|s| *s > 0).map(|s| s as f32),
            _ => wifi.tx_mbps,
        };
        let signal_dbm =
            fs::read_to_string("/proc/net/wireless").ok().and_then(|t| parse_wireless(&t, &interface));
        let ipv6 = fs::read_to_string("/proc/net/if_inet6").ok().and_then(|t| parse_if_inet6(&t, &interface));
        let gateway = default_route(&route).filter(|(name, _)| *name == interface).map(|(_, gw)| gw);
        Some(NetSample {
            ipv4: ipv4_of(&interface),
            interface,
            kind,
            ssid: wifi.ssid,
            frequency_mhz: wifi.frequency_mhz,
            signal_dbm,
            link_mbps,
            ipv6,
            gateway,
            down_rate: rates.down,
            up_rate: rates.up,
            session_down: rates.session.0,
            session_up: rates.session.1,
            total_down: counters.0,
            total_up: counters.1,
        })
    }
}

/// The interface and gateway of the default IPv4 route with the lowest
/// metric, from /proc/net/route.
fn default_route(text: &str) -> Option<(String, Ipv4Addr)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            // Iface Destination Gateway Flags RefCnt Use Metric Mask ...
            if f.len() < 8 || f[1] != "00000000" || f[7] != "00000000" {
                return None;
            }
            let gateway = u32::from_str_radix(f[2], 16).ok()?;
            let metric: u32 = f[6].parse().ok()?;
            // Addresses are in network byte order, printed as a host
            // (little-endian) integer.
            Some((metric, f[0].to_string(), Ipv4Addr::from(gateway.to_le_bytes())))
        })
        .min_by_key(|(metric, ..)| *metric)
        .map(|(_, name, gateway)| (name, gateway))
}

/// (received, sent) bytes for `interface` from /proc/net/dev.
fn parse_net_dev(text: &str, interface: &str) -> Option<(u64, u64)> {
    text.lines().find_map(|line| {
        let (name, rest) = line.split_once(':')?;
        if name.trim() != interface {
            return None;
        }
        let f: Vec<u64> = rest.split_whitespace().map(|v| v.parse().ok()).collect::<Option<_>>()?;
        Some((*f.first()?, *f.get(8)?))
    })
}

/// Signal level in dBm from /proc/net/wireless.
fn parse_wireless(text: &str, interface: &str) -> Option<i32> {
    text.lines().find_map(|line| {
        let (name, rest) = line.split_once(':')?;
        if name.trim() != interface {
            return None;
        }
        // status link level noise ...
        let level: f32 = rest.split_whitespace().nth(2)?.trim_end_matches('.').parse().ok()?;
        // Drivers without dBm support report 0 or a positive quality.
        (level < 0.0).then_some(level as i32)
    })
}

/// A global IPv6 address of `interface` from /proc/net/if_inet6,
/// preferring stable addresses over temporary (privacy) ones.
fn parse_if_inet6(text: &str, interface: &str) -> Option<Ipv6Addr> {
    const SCOPE_GLOBAL: u32 = 0x00;
    const TEMPORARY: u32 = 0x01;
    let mut addresses: Vec<(bool, Ipv6Addr)> = text
        .lines()
        .filter_map(|line| {
            // address ifindex prefixlen scope flags name
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 6 || f[5] != interface || u32::from_str_radix(f[3], 16).ok()? != SCOPE_GLOBAL {
                return None;
            }
            let flags = u32::from_str_radix(f[4], 16).ok()?;
            Some((flags & TEMPORARY != 0, Ipv6Addr::from(u128::from_str_radix(f[0], 16).ok()?)))
        })
        .collect();
    addresses.sort_by_key(|(temporary, _)| *temporary);
    addresses.first().map(|(_, a)| *a)
}

fn ipv4_of(interface: &str) -> Option<Ipv4Addr> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list we only read and
    // free with freeifaddrs; ifa_addr is checked for null and is a
    // sockaddr_in when its family is AF_INET; ifa_name is NUL-terminated.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return None;
        }
        let mut found = None;
        let mut p = list;
        while !p.is_null() {
            let ifa = &*p;
            if !ifa.ifa_addr.is_null()
                && i32::from((*ifa.ifa_addr).sa_family) == libc::AF_INET
                && CStr::from_ptr(ifa.ifa_name).to_bytes() == interface.as_bytes()
            {
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                found = Some(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)));
                break;
            }
            p = ifa.ifa_next;
        }
        libc::freeifaddrs(list);
        found
    }
}

/// Parses `iw dev <if> link` output; empty when not connected.
fn parse_iw_link(text: &str) -> WifiLink {
    let value = |key: &str| text.lines().find_map(|l| l.trim().strip_prefix(key).map(str::trim));
    WifiLink {
        ssid: value("SSID:").map(unescape_ssid),
        frequency_mhz: value("freq:").and_then(|f| f.parse::<f32>().ok()).map(|f| f as u32),
        tx_mbps: value("tx bitrate:").and_then(|b| b.split_whitespace().next()?.parse().ok()),
    }
}

/// `iw` prints non-ASCII SSID bytes as `\xNN`.
fn unescape_ssid(ssid: &str) -> String {
    let mut bytes = Vec::with_capacity(ssid.len());
    let mut rest = ssid.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        if b == b'\\'
            && let [b'x', h, l, tail @ ..] = tail
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&[*h, *l]).unwrap_or(""), 16)
        {
            bytes.push(v);
            rest = tail;
            continue;
        }
        bytes.push(b);
        rest = tail;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_default_route() {
        let text = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                    wlan0\t00000000\t0107A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                    eth0\t00000000\t0100000A\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                    eth0\t0000000A\t00000000\t0001\t0\t0\t100\t000000FF\t0\t0\t0\n";
        assert_eq!(default_route(text), Some(("eth0".into(), Ipv4Addr::new(10, 0, 0, 1))));
        let wifi_only = text.lines().take(2).collect::<Vec<_>>().join("\n");
        assert_eq!(default_route(&wifi_only), Some(("wlan0".into(), Ipv4Addr::new(192, 168, 7, 1))));
        assert_eq!(default_route("Iface\tDestination\n"), None);
    }

    #[test]
    fn parses_counters_and_signal() {
        let dev = "Inter-|   Receive\n face |bytes\n    lo: 10 1 0 0 0 0 0 0 10 1 0 0 0 0 0 0\n\
                   wlan0: 104635934  109347    0    0    0     0          0         0 24032057   47968    0    4    0     0       0          0\n";
        assert_eq!(parse_net_dev(dev, "wlan0"), Some((104_635_934, 24_032_057)));
        assert_eq!(parse_net_dev(dev, "eth0"), None);

        let wireless = "Inter-| sta-|   Quality\n face | tus | link level noise\nwlan0: 0000   58.  -52.  -256        0      0\n";
        assert_eq!(parse_wireless(wireless, "wlan0"), Some(-52));
        assert_eq!(parse_wireless("wlan0: 0000 70. 0. 0\n", "wlan0"), None);
    }

    #[test]
    fn picks_stable_global_ipv6() {
        let text = "fe80000000000000c97f3a56151864dd 03 40 20 80 wlan0\n\
                    20010db8000000000000000000000099 03 40 00 01 wlan0\n\
                    20010db8000000000000000000000001 03 40 00 00 wlan0\n\
                    20010db8000000000000000000000002 04 40 00 00 eth0\n";
        assert_eq!(parse_if_inet6(text, "wlan0"), Some("2001:db8::1".parse().unwrap()));
        assert_eq!(parse_if_inet6(&text.lines().take(1).collect::<String>(), "wlan0"), None);
    }

    #[test]
    fn parses_iw_link() {
        let text = "Connected to 02:00:00:00:00:01 (on wlan0)\n\tSSID: Caf\\xc3\\xa9 Net\n\tfreq: 5180.0\n\
                    \tsignal: -52 dBm\n\ttx bitrate: 780.0 MBit/s VHT-MCS 8 80MHz short GI VHT-NSS 2\n";
        let link = parse_iw_link(text);
        assert_eq!(link, WifiLink { ssid: Some("Café Net".into()), frequency_mhz: Some(5180), tx_mbps: Some(780.0) });
        assert_eq!(parse_iw_link("Not connected.\n"), WifiLink::default());
    }

    #[test]
    fn reads_live_network() {
        let mut net = network(None);
        // Offline machines (CI) have no default route.
        if let Some(first) = net.sample() {
            assert!(first.down_rate.is_none());
            let second = net.sample().expect("still online");
            assert!(second.down_rate.is_some() && second.total_down >= first.total_down);
        }
    }
}
