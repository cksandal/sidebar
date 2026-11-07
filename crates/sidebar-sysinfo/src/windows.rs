//! Windows sources, through Win32 (windows-sys). The arithmetic is kept in
//! small pure functions with tests; the API calls around them only fill
//! structs.
//!
//! CPU temperature has no reliable user-mode API on Windows (WMI's
//! `MSAcpi_ThermalZoneTemperature` needs admin rights and is usually
//! missing), so samples carry none and the CPU widget hides that chart.

use std::mem::{size_of, zeroed};

use sidebar_core::Source;
use sidebar_core::source::{Battery, CoreGroup, CpuInfo, CpuSample, Memory, UtcOffset};
use windows_sys::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessorPerformanceInformation};
use windows_sys::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW,
    PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows_sys::Win32::System::Power::{CallNtPowerInformation, GetSystemPowerStatus, PROCESSOR_POWER_INFORMATION, ProcessorInformation, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows_sys::Win32::System::SystemInformation::{
    CpuSetInformation, GetSystemCpuSetInformation, GetSystemInfo, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_CPU_SET_INFORMATION, SYSTEM_INFO,
};
use windows_sys::Win32::System::Time::{DYNAMIC_TIME_ZONE_INFORMATION, GetDynamicTimeZoneInformation, TIME_ZONE_ID_INVALID};
use windows_sys::Win32::System::WindowsProgramming::SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION;

pub use crate::net_windows::network;
pub use crate::nvml::gpus;

/// Null-terminated UTF-16, for Win32 string arguments.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// UTF-16 up to the first NUL.
pub(crate) fn from_wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

// ---------------------------------------------------------------- CPU

pub fn cpu() -> Box<dyn Source<CpuSample>> {
    Box::new(CpuSource { prev: Vec::new(), perf: PerfCounter::open(r"\Processor Information(_Total)\% Processor Performance"), base_mhz: base_mhz() })
}

pub fn cpu_info() -> CpuInfo {
    let cores = processor_count();
    let groups = efficiency_classes().map(|classes| core_groups(&classes, cores)).unwrap_or_default();
    CpuInfo { model: cpu_model(), cores, groups, temperature: false }
}

struct CpuSource {
    /// (busy, total) 100 ns ticks per core at the previous sample.
    prev: Vec<(u64, u64)>,
    /// Current performance relative to the base clock, in percent. This
    /// is what Task Manager's "Speed" uses; `CurrentMhz` from the power
    /// API is stuck at the base clock on most machines.
    perf: Option<PerfCounter>,
    base_mhz: Option<f32>,
}

impl Source<CpuSample> for CpuSource {
    fn sample(&mut self) -> Option<CpuSample> {
        let now = core_times()?;
        let prev = std::mem::replace(&mut self.prev, now.clone());
        let (total, cores) = loads(&prev, &now)?;
        let percent = self.perf.as_mut().and_then(PerfCounter::read);
        let mhz = match (percent, self.base_mhz) {
            (Some(p), Some(base)) if p > 0.0 => Some(base * p as f32 / 100.0),
            _ => current_mhz(),
        };
        Some(CpuSample { total, cores, mhz, celsius: None })
    }
}

/// Load over all cores and per core between two readings of (busy,
/// total) ticks; `None` if there is no previous reading or the number of
/// cores changed.
fn loads(prev: &[(u64, u64)], now: &[(u64, u64)]) -> Option<(f32, Vec<f32>)> {
    if prev.len() != now.len() || now.is_empty() {
        return None;
    }
    let deltas: Vec<(u64, u64)> = now.iter().zip(prev).map(|(n, p)| (n.0.saturating_sub(p.0), n.1.saturating_sub(p.1))).collect();
    let fraction = |(busy, total): (u64, u64)| if total > 0 { (busy as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
    let sum = deltas.iter().fold((0, 0), |a, d| (a.0 + d.0, a.1 + d.1));
    Some((fraction(sum), deltas.into_iter().map(fraction).collect()))
}

/// Logical processors in the current processor group (all of them on
/// machines with up to 64).
fn processor_count() -> usize {
    // SAFETY: GetSystemInfo only fills the struct.
    let info: SYSTEM_INFO = unsafe {
        let mut info = zeroed();
        GetSystemInfo(&mut info);
        info
    };
    info.dwNumberOfProcessors as usize
}

/// (busy, total) 100 ns ticks per logical processor, in processor order.
fn core_times() -> Option<Vec<(u64, u64)>> {
    let n = processor_count();
    let mut buf = vec![SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION::default(); n];
    let len = (n * size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>()) as u32;
    let mut returned = 0;
    // SAFETY: the buffer holds `n` entries and its length in bytes is passed.
    let status = unsafe { NtQuerySystemInformation(SystemProcessorPerformanceInformation, buf.as_mut_ptr().cast(), len, &mut returned) };
    if status < 0 {
        return None;
    }
    let count = returned as usize / size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>();
    // Kernel time includes idle time.
    Some(buf[..count].iter().map(|p| (busy_ticks(p.KernelTime, p.UserTime, p.IdleTime), (p.KernelTime + p.UserTime) as u64)).collect())
}

fn busy_ticks(kernel: i64, user: i64, idle: i64) -> u64 {
    (kernel + user - idle).max(0) as u64
}

fn power_info() -> Vec<PROCESSOR_POWER_INFORMATION> {
    let n = processor_count();
    let mut buf: Vec<PROCESSOR_POWER_INFORMATION> = vec![PROCESSOR_POWER_INFORMATION::default(); n];
    let len = (n * size_of::<PROCESSOR_POWER_INFORMATION>()) as u32;
    // SAFETY: the output buffer holds `n` entries; no input buffer.
    let status = unsafe { CallNtPowerInformation(ProcessorInformation, std::ptr::null(), 0, buf.as_mut_ptr().cast(), len) };
    if status < 0 { Vec::new() } else { buf }
}

fn base_mhz() -> Option<f32> {
    power_info().iter().map(|p| p.MaxMhz).max().filter(|&m| m > 0).map(|m| m as f32)
}

fn current_mhz() -> Option<f32> {
    let info = power_info();
    (!info.is_empty()).then(|| info.iter().map(|p| p.CurrentMhz as f32).sum::<f32>() / info.len() as f32)
}

fn cpu_model() -> Option<String> {
    let key = wide(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0");
    let value = wide("ProcessorNameString");
    let mut buf = [0u16; 256];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: NUL-terminated names; the buffer size in bytes is passed.
    let err = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len)
    };
    let name = from_wide(&buf).trim().to_string();
    (err == 0 && !name.is_empty()).then_some(name)
}

/// (logical processor index, efficiency class) for every CPU set in
/// processor group 0.
fn efficiency_classes() -> Option<Vec<(usize, u8)>> {
    let mut len = 0u32;
    // SAFETY: a null buffer with length 0 asks for the required length.
    unsafe { GetSystemCpuSetInformation(std::ptr::null_mut(), 0, &mut len, std::ptr::null_mut(), 0) };
    if len == 0 {
        return None;
    }
    // u64 elements keep the buffer aligned for the structs inside.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: the buffer is at least `len` bytes.
    let ok = unsafe { GetSystemCpuSetInformation(buf.as_mut_ptr().cast(), len, &mut len, std::ptr::null_mut(), 0) };
    if ok == 0 {
        return None;
    }
    let bytes = buf.as_ptr().cast::<u8>();
    let mut classes = Vec::new();
    let mut offset = 0usize;
    while offset + size_of::<SYSTEM_CPU_SET_INFORMATION>() <= len as usize {
        // SAFETY: entries are laid out back to back, each `Size` bytes
        // long, within the `len` bytes the call filled; 8-byte alignment
        // holds because every Size is a multiple of 8.
        let entry = unsafe { &*bytes.add(offset).cast::<SYSTEM_CPU_SET_INFORMATION>() };
        if entry.Size == 0 {
            break;
        }
        if entry.Type == CpuSetInformation {
            // SAFETY: Type says the union holds a CpuSet.
            let set = unsafe { entry.Anonymous.CpuSet };
            if set.Group == 0 {
                classes.push((set.LogicalProcessorIndex as usize, set.EfficiencyClass));
            }
        }
        offset += entry.Size as usize;
    }
    Some(classes)
}

/// P-cores and E-cores from efficiency classes: on hybrid CPUs there are
/// two, and the higher class is the faster core.
fn core_groups(classes: &[(usize, u8)], cores: usize) -> Vec<CoreGroup> {
    let mut distinct: Vec<u8> = classes.iter().map(|c| c.1).collect();
    distinct.sort_unstable();
    distinct.dedup();
    let [low, high] = distinct[..] else { return Vec::new() };
    let pick = |class: u8| -> Vec<usize> {
        let mut v: Vec<usize> = classes.iter().filter(|c| c.1 == class && c.0 < cores).map(|c| c.0).collect();
        v.sort_unstable();
        v
    };
    vec![CoreGroup { name: "P-cores".into(), cores: pick(high) }, CoreGroup { name: "E-cores".into(), cores: pick(low) }]
}

/// One PDH counter in its own query.
struct PerfCounter {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

impl PerfCounter {
    fn open(path: &str) -> Option<Self> {
        let path = wide(path);
        let (mut query, mut counter) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: out pointers are valid; the counter path is the English
        // name, so it works on any display language.
        unsafe {
            if PdhOpenQueryW(std::ptr::null(), 0, &mut query) != 0 {
                return None;
            }
            if PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) != 0 {
                PdhCloseQuery(query);
                return None;
            }
            // Rate counters need two collections; this is the first.
            PdhCollectQueryData(query);
        }
        Some(Self { query, counter })
    }

    fn read(&mut self) -> Option<f64> {
        // SAFETY: handles from `open`; `value` is a valid out pointer.
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            let mut value: PDH_FMT_COUNTERVALUE = zeroed();
            if PdhGetFormattedCounterValue(self.counter, PDH_FMT_DOUBLE, std::ptr::null_mut(), &mut value) != 0 {
                return None;
            }
            matches!(value.CStatus, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA).then(|| value.Anonymous.doubleValue)
        }
    }
}

impl Drop for PerfCounter {
    fn drop(&mut self) {
        // SAFETY: the query is open and closed only here.
        unsafe { PdhCloseQuery(self.query) };
    }
}

// ---------------------------------------------------------------- memory

pub fn memory() -> Box<dyn Source<Memory>> {
    Box::new(|| {
        // SAFETY: dwLength is set as the API requires; the call only fills
        // the struct.
        let status = unsafe {
            let mut status: MEMORYSTATUSEX = zeroed();
            status.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut status) == 0 {
                return None;
            }
            status
        };
        Some(memory_from(status.ullTotalPhys, status.ullAvailPhys, status.ullTotalPageFile, status.ullAvailPageFile))
    })
}

/// The "page file" figures are the commit limit and what is left of it,
/// both of which include RAM; the page file itself is the difference.
fn memory_from(total_phys: u64, avail_phys: u64, commit_limit: u64, commit_avail: u64) -> Memory {
    let used = total_phys.saturating_sub(avail_phys);
    let committed = commit_limit.saturating_sub(commit_avail);
    let swap_total = commit_limit.saturating_sub(total_phys);
    Memory {
        used_bytes: used,
        total_bytes: total_phys,
        swap_used_bytes: committed.saturating_sub(used).min(swap_total),
        swap_total_bytes: swap_total,
    }
}

// ---------------------------------------------------------------- battery

/// `BatteryFlag` bits.
const BATTERY_CHARGING: u8 = 8;
const NO_SYSTEM_BATTERY: u8 = 128;
const UNKNOWN: u8 = 255;

fn power_status() -> Option<SYSTEM_POWER_STATUS> {
    // SAFETY: only fills the struct.
    unsafe {
        let mut status = zeroed();
        (GetSystemPowerStatus(&mut status) != 0).then_some(status)
    }
}

pub fn battery() -> Option<Box<dyn Source<Battery>>> {
    let flag = power_status()?.BatteryFlag;
    if flag == UNKNOWN || flag & NO_SYSTEM_BATTERY != 0 {
        return None;
    }
    Some(Box::new(|| battery_from(&power_status()?)))
}

fn battery_from(s: &SYSTEM_POWER_STATUS) -> Option<Battery> {
    if s.BatteryLifePercent > 100 {
        return None;
    }
    Some(Battery { level: s.BatteryLifePercent as f32 / 100.0, charging: s.BatteryFlag != UNKNOWN && s.BatteryFlag & BATTERY_CHARGING != 0 })
}

// ---------------------------------------------------------------- time zone

const TIME_ZONE_ID_STANDARD: u32 = 1;
const TIME_ZONE_ID_DAYLIGHT: u32 = 2;

pub fn utc_offset() -> Box<dyn Source<UtcOffset>> {
    Box::new(|| {
        // SAFETY: only fills the struct.
        let (id, tz) = unsafe {
            let mut tz: DYNAMIC_TIME_ZONE_INFORMATION = zeroed();
            (GetDynamicTimeZoneInformation(&mut tz), tz)
        };
        (id != TIME_ZONE_ID_INVALID).then(|| offset_from(id, tz.Bias, tz.StandardBias, tz.DaylightBias))
    })
}

/// Windows biases are minutes to add to local time to get UTC.
fn offset_from(id: u32, bias: i32, standard: i32, daylight: i32) -> UtcOffset {
    let extra = match id {
        TIME_ZONE_ID_DAYLIGHT => daylight,
        TIME_ZONE_ID_STANDARD => standard,
        _ => 0,
    };
    UtcOffset(-(bias + extra) * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_loads() {
        assert_eq!(loads(&[], &[(1, 2)]), None);
        let prev = [(100, 1000), (0, 1000)];
        let now = [(600, 2000), (0, 2000)];
        assert_eq!(loads(&prev, &now), Some((0.25, vec![0.5, 0.0])));
        assert_eq!(busy_ticks(500, 300, 200), 600);
        assert_eq!(busy_ticks(100, 0, 200), 0);
    }

    #[test]
    fn splits_hybrid_cores() {
        // Two P-cores with two threads each, then four E-cores.
        let classes: Vec<(usize, u8)> = (0..4).map(|i| (i, 1)).chain((4..8).map(|i| (i, 0))).collect();
        let groups = core_groups(&classes, 8);
        assert_eq!(groups[0], CoreGroup { name: "P-cores".into(), cores: vec![0, 1, 2, 3] });
        assert_eq!(groups[1], CoreGroup { name: "E-cores".into(), cores: vec![4, 5, 6, 7] });
        assert!(core_groups(&[(0, 0), (1, 0)], 2).is_empty());
    }

    #[test]
    fn page_file_excludes_ram() {
        const G: u64 = 1 << 30;
        let m = memory_from(16 * G, 6 * G, 20 * G, 8 * G);
        assert_eq!(m, Memory { used_bytes: 10 * G, total_bytes: 16 * G, swap_used_bytes: 2 * G, swap_total_bytes: 4 * G });
        // No page file.
        let m = memory_from(16 * G, 6 * G, 16 * G, 6 * G);
        assert_eq!((m.swap_used_bytes, m.swap_total_bytes), (0, 0));
    }

    #[test]
    fn converts_time_zone_bias() {
        // Pacific: bias 480, daylight bias -60.
        assert_eq!(offset_from(TIME_ZONE_ID_DAYLIGHT, 480, 0, -60), UtcOffset(-7 * 3600));
        assert_eq!(offset_from(TIME_ZONE_ID_STANDARD, 480, 0, -60), UtcOffset(-8 * 3600));
        // India, no DST.
        assert_eq!(offset_from(0, -330, 0, 0), UtcOffset(5 * 3600 + 1800));
    }

    #[test]
    fn reads_battery_status() {
        let s = |flag, percent| SYSTEM_POWER_STATUS { ACLineStatus: 1, BatteryFlag: flag, BatteryLifePercent: percent, ..Default::default() };
        assert_eq!(battery_from(&s(BATTERY_CHARGING, 40)), Some(Battery { level: 0.4, charging: true }));
        assert_eq!(battery_from(&s(1, 90)), Some(Battery { level: 0.9, charging: false }));
        assert_eq!(battery_from(&s(UNKNOWN, 255)), None);
    }

    #[test]
    fn reads_live_system() {
        assert!(memory().sample().is_some_and(|m| m.used_bytes <= m.total_bytes && m.total_bytes > 0));
        let info = cpu_info();
        assert!(info.cores > 0);
        assert!(info.model.is_some());
        let mut cpu = cpu();
        assert!(cpu.sample().is_none(), "first sample has nothing to compare against");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let s = cpu.sample().expect("second sample");
        assert_eq!(s.cores.len(), info.cores);
        assert!((0.0..=1.0).contains(&s.total));
        assert!(utc_offset().sample().is_some_and(|o| o.0.abs() <= 14 * 3600));
    }
}
