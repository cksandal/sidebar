//! Network source for Windows: IP Helper for adapters, addresses, routes
//! and counters, and the WLAN API for Wi-Fi details.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

use sidebar_core::Source;
use sidebar_core::source::{LinkKind, NetSample};
use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, HANDLE, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, GetBestInterface,
    GetIfEntry2, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211, IP_ADAPTER_ADDRESSES_LH, MIB_IF_ROW2,
};
use windows_sys::Win32::NetworkManagement::WiFi::{
    DOT11_SSID, WLAN_BSS_LIST, WLAN_CONNECTION_ATTRIBUTES, WlanCloseHandle, WlanFreeMemory, WlanGetNetworkBssList, WlanOpenHandle,
    WlanQueryInterface, dot11_BSS_type_any, wlan_interface_state_connected, wlan_intf_opcode_current_connection,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, IpSuffixOriginRandom, SOCKET_ADDRESS};
use windows_sys::core::GUID;

use crate::traffic::Traffic;

/// How often the Wi-Fi details are read again.
const WIFI_REFRESH: Duration = Duration::from_secs(5);
/// A public address to ask the routing table about; nothing is sent.
const PROBE: Ipv4Addr = Ipv4Addr::new(8, 8, 8, 8);

/// `interface` pins the adapter (by its name in Settings, e.g. "Wi-Fi" or
/// "Ethernet 2"); otherwise the one with the default route is followed.
pub fn network(interface: Option<String>) -> Option<Box<dyn Source<NetSample>>> {
    Some(Box::new(NetSource { pinned: interface, traffic: Traffic::default(), wlan: None, wifi: None }))
}

struct NetSource {
    pinned: Option<String>,
    traffic: Traffic,
    /// Opened on the first Wi-Fi sample; `None` inside if there is no WLAN
    /// service.
    wlan: Option<Option<Wlan>>,
    /// Wi-Fi details for an interface, and when they were read.
    wifi: Option<(GUID, WifiLink, Instant)>,
}

/// The parts of an adapter the sample needs, copied out of the
/// GetAdaptersAddresses buffer.
struct Adapter {
    name: String,
    if_type: u32,
    ipv4: Option<Ipv4Addr>,
    ipv6: Option<Ipv6Addr>,
    gateway: Option<Ipv4Addr>,
    row: MIB_IF_ROW2,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct WifiLink {
    ssid: Option<String>,
    frequency_mhz: Option<u32>,
    signal_dbm: Option<i32>,
    tx_mbps: Option<f32>,
}

impl Source<NetSample> for NetSource {
    /// `None` while offline (no default route) or when the adapter is gone.
    fn sample(&mut self) -> Option<NetSample> {
        let adapter = find_adapter(self.pinned.as_deref())?;
        let row = &adapter.row;
        let counters = (row.InOctets, row.OutOctets);
        let rates = self.traffic.update(&adapter.name, counters);
        // Bit 0 of the flags: a physical adapter (not Hyper-V, VPN, ...).
        let hardware = row.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
        let kind = match adapter.if_type {
            IF_TYPE_IEEE80211 => LinkKind::Wireless,
            IF_TYPE_ETHERNET_CSMACD if hardware => LinkKind::Wired,
            _ => LinkKind::Other,
        };
        let wifi = if kind == LinkKind::Wireless { self.wifi_link(row.InterfaceGuid) } else { WifiLink::default() };
        let link_mbps = match kind {
            LinkKind::Wireless => wifi.tx_mbps,
            _ => link_speed(row.TransmitLinkSpeed),
        };
        Some(NetSample {
            interface: adapter.name,
            kind,
            ssid: wifi.ssid,
            frequency_mhz: wifi.frequency_mhz,
            signal_dbm: wifi.signal_dbm,
            link_mbps,
            ipv4: adapter.ipv4,
            ipv6: adapter.ipv6,
            gateway: adapter.gateway,
            down_rate: rates.down,
            up_rate: rates.up,
            session_down: rates.session.0,
            session_up: rates.session.1,
            total_down: counters.0,
            total_up: counters.1,
        })
    }
}

impl NetSource {
    fn wifi_link(&mut self, guid: GUID) -> WifiLink {
        let same = |g: &GUID| g.data1 == guid.data1 && g.data2 == guid.data2 && g.data3 == guid.data3 && g.data4 == guid.data4;
        if let Some((g, link, at)) = &self.wifi
            && same(g)
            && at.elapsed() < WIFI_REFRESH
        {
            return link.clone();
        }
        let wlan = self.wlan.get_or_insert_with(Wlan::open);
        let link = wlan.as_ref().and_then(|w| w.link(&guid)).unwrap_or_default();
        self.wifi = Some((guid, link.clone(), Instant::now()));
        link
    }
}

/// Link speed in bits per second as Mbit/s; unknown speeds are reported
/// as 0 or all ones.
fn link_speed(bps: u64) -> Option<f32> {
    (bps > 0 && bps != u64::MAX).then_some((bps / 1_000_000) as f32)
}

fn find_adapter(pinned: Option<&str>) -> Option<Adapter> {
    let best = match pinned {
        Some(_) => None,
        None => {
            let mut index = 0;
            // The address is in network byte order, as stored in memory.
            // SAFETY: valid out pointer.
            if unsafe { GetBestInterface(u32::from_ne_bytes(PROBE.octets()), &mut index) } != NO_ERROR {
                return None;
            }
            Some(index)
        }
    };
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let buf = adapters_addresses(flags)?;
    let mut node = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    while !node.is_null() {
        // SAFETY: a node of the linked list GetAdaptersAddresses built in
        // `buf`, which outlives this loop.
        let a = unsafe { &*node };
        node = a.Next;
        // SAFETY: FriendlyName is a NUL-terminated wide string.
        let name = unsafe { wide_ptr(a.FriendlyName) };
        // SAFETY: the union holds Length and IfIndex on every Windows
        // version that has GetAdaptersAddresses.
        let index = unsafe { a.Anonymous1.Anonymous.IfIndex };
        let wanted = match (pinned, best) {
            (Some(p), _) => name.eq_ignore_ascii_case(p),
            (None, Some(b)) => index == b,
            (None, None) => false,
        };
        if !wanted {
            continue;
        }
        // SAFETY: zeroed is a valid MIB_IF_ROW2; GetIfEntry2 reads the LUID
        // and fills the rest.
        let row = unsafe {
            let mut row: MIB_IF_ROW2 = std::mem::zeroed();
            row.InterfaceLuid = a.Luid;
            if GetIfEntry2(&mut row) != NO_ERROR {
                return None;
            }
            row
        };
        let (mut ipv4, mut ipv6, mut ipv6_temp) = (None, None, None);
        let mut u = a.FirstUnicastAddress;
        while !u.is_null() {
            // SAFETY: list nodes inside `buf`.
            let ua = unsafe { &*u };
            u = ua.Next;
            match socket_ip(&ua.Address) {
                Some(IpAddr::V4(v4)) if ipv4.is_none() => ipv4 = Some(v4),
                Some(IpAddr::V6(v6)) if is_global(&v6) => {
                    let slot = if ua.SuffixOrigin == IpSuffixOriginRandom { &mut ipv6_temp } else { &mut ipv6 };
                    slot.get_or_insert(v6);
                }
                _ => {}
            }
        }
        let mut gateway = None;
        let mut g = a.FirstGatewayAddress;
        while !g.is_null() && gateway.is_none() {
            // SAFETY: list nodes inside `buf`.
            let ga = unsafe { &*g };
            g = ga.Next;
            if let Some(IpAddr::V4(v4)) = socket_ip(&ga.Address) {
                gateway = Some(v4);
            }
        }
        return Some(Adapter { name, if_type: a.IfType, ipv4, ipv6: ipv6.or(ipv6_temp), gateway, row });
    }
    None
}

/// The GetAdaptersAddresses list, in a u64 buffer for alignment.
fn adapters_addresses(flags: u32) -> Option<Vec<u64>> {
    let mut len: u32 = 16 * 1024;
    // The list can grow between calls; retry a few times.
    for _ in 0..4 {
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: the buffer is `len` bytes.
        let err = unsafe { GetAdaptersAddresses(AF_UNSPEC as u32, flags, std::ptr::null(), buf.as_mut_ptr().cast(), &mut len) };
        match err {
            NO_ERROR => return Some(buf),
            ERROR_BUFFER_OVERFLOW => continue,
            _ => return None,
        }
    }
    None
}

/// # Safety
/// `p` is null or a NUL-terminated UTF-16 string.
unsafe fn wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: NUL-terminated, per the caller.
    unsafe {
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

fn socket_ip(addr: &SOCKET_ADDRESS) -> Option<IpAddr> {
    if addr.lpSockaddr.is_null() || addr.iSockaddrLength <= 0 {
        return None;
    }
    // SAFETY: the API gives a sockaddr of iSockaddrLength bytes.
    let bytes = unsafe { std::slice::from_raw_parts(addr.lpSockaddr.cast::<u8>(), addr.iSockaddrLength as usize) };
    sockaddr_ip(bytes)
}

/// The address in a raw sockaddr_in or sockaddr_in6.
fn sockaddr_ip(b: &[u8]) -> Option<IpAddr> {
    let family = u16::from_ne_bytes([*b.first()?, *b.get(1)?]);
    match family {
        AF_INET => Some(IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(b.get(4..8)?).ok()?))),
        AF_INET6 => Some(IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(b.get(8..24)?).ok()?))),
        _ => None,
    }
}

/// Global unicast: not link-local, loopback, multicast or unspecified.
/// Unique local addresses (fc00::/7) count, as on Linux.
fn is_global(a: &Ipv6Addr) -> bool {
    let first = a.segments()[0];
    !(a.is_loopback() || a.is_unspecified() || a.is_multicast() || first & 0xffc0 == 0xfe80)
}

/// A WLAN client handle.
struct Wlan(HANDLE);

impl Wlan {
    fn open() -> Option<Self> {
        let (mut version, mut handle) = (0, std::ptr::null_mut());
        // SAFETY: valid out pointers; client version 2 is Vista and later.
        (unsafe { WlanOpenHandle(2, std::ptr::null(), &mut version, &mut handle) } == NO_ERROR).then_some(Self(handle))
    }

    fn link(&self, guid: &GUID) -> Option<WifiLink> {
        let mut size = 0;
        let mut data: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: valid handle and out pointers; WLAN memory is freed
        // below with WlanFreeMemory.
        let attrs = unsafe {
            if WlanQueryInterface(self.0, guid, wlan_intf_opcode_current_connection, std::ptr::null(), &mut size, &mut data, std::ptr::null_mut())
                != NO_ERROR
            {
                return None;
            }
            let attrs = *data.cast::<WLAN_CONNECTION_ATTRIBUTES>();
            WlanFreeMemory(data);
            attrs
        };
        if attrs.isState != wlan_interface_state_connected {
            return None;
        }
        let assoc = attrs.wlanAssociationAttributes;
        let ssid = &assoc.dot11Ssid;
        let mut link = WifiLink {
            ssid: Some(ssid_text(ssid)),
            frequency_mhz: None,
            // Quality is 0..100, linear between -100 and -50 dBm.
            signal_dbm: Some(assoc.wlanSignalQuality.min(100) as i32 / 2 - 100),
            tx_mbps: (assoc.ulTxRate > 0).then(|| assoc.ulTxRate as f32 / 1000.0),
        };
        // The scan list has the real RSSI and the channel frequency.
        let mut list: *mut WLAN_BSS_LIST = std::ptr::null_mut();
        // SAFETY: as above; the list is freed with WlanFreeMemory.
        unsafe {
            if WlanGetNetworkBssList(self.0, guid, ssid, dot11_BSS_type_any, 0, std::ptr::null(), &mut list) == NO_ERROR {
                let n = (*list).dwNumberOfItems as usize;
                let entries = std::slice::from_raw_parts((*list).wlanBssEntries.as_ptr(), n);
                if let Some(bss) = entries.iter().find(|e| e.dot11Bssid == assoc.dot11Bssid) {
                    link.signal_dbm = Some(bss.lRssi);
                    link.frequency_mhz = Some(bss.ulChCenterFrequency / 1000);
                }
                WlanFreeMemory(list.cast());
            }
        }
        Some(link)
    }
}

impl Drop for Wlan {
    fn drop(&mut self) {
        // SAFETY: the handle is open and closed only here.
        unsafe { WlanCloseHandle(self.0, std::ptr::null()) };
    }
}

fn ssid_text(ssid: &DOT11_SSID) -> String {
    let len = (ssid.uSSIDLength as usize).min(ssid.ucSSID.len());
    String::from_utf8_lossy(&ssid.ucSSID[..len]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sockaddrs() {
        let mut v4 = [0u8; 16];
        v4[..2].copy_from_slice(&AF_INET.to_ne_bytes());
        v4[4..8].copy_from_slice(&[192, 168, 1, 20]);
        assert_eq!(sockaddr_ip(&v4), Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20))));
        let mut v6 = [0u8; 28];
        v6[..2].copy_from_slice(&AF_INET6.to_ne_bytes());
        let addr: Ipv6Addr = "2001:db8::1".parse().unwrap();
        v6[8..24].copy_from_slice(&addr.octets());
        assert_eq!(sockaddr_ip(&v6), Some(IpAddr::V6(addr)));
        assert_eq!(sockaddr_ip(&v4[..6]), None);
        assert_eq!(sockaddr_ip(&[]), None);
    }

    #[test]
    fn classifies_ipv6() {
        assert!(is_global(&"2001:db8::1".parse().unwrap()));
        assert!(is_global(&"fd12::1".parse().unwrap()));
        assert!(!is_global(&"fe80::1".parse().unwrap()));
        assert!(!is_global(&Ipv6Addr::LOCALHOST));
        assert_eq!(link_speed(1_000_000_000), Some(1000.0));
        assert_eq!(link_speed(u64::MAX), None);
        let mut ssid = DOT11_SSID { uSSIDLength: 4, ucSSID: [0; 32] };
        ssid.ucSSID[..4].copy_from_slice(b"home");
        assert_eq!(ssid_text(&ssid), "home");
    }

    #[test]
    fn reads_live_network() {
        let mut net = network(None).unwrap();
        // The test VM has a default route; on a machine without one this
        // is None and there is nothing to check.
        if let Some(s) = net.sample() {
            assert!(!s.interface.is_empty());
            assert!(s.down_rate.is_none());
        }
    }
}
