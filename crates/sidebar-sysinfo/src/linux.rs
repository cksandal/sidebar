//! Linux sources: procfs and sysfs text files, plus libc for local time.
//! Parsing is split from file access so it can be tested with fixtures.

use std::fs;
use std::path::{Path, PathBuf};

use sidebar_core::Source;
use sidebar_core::source::{Battery, CoreGroup, CpuInfo, CpuSample, Memory, UtcOffset};

const POWER_SUPPLY: &str = "/sys/class/power_supply";
const HWMON: &str = "/sys/class/hwmon";
const THERMAL: &str = "/sys/class/thermal";

pub fn cpu() -> Box<dyn Source<CpuSample>> {
    Box::new(CpuSource { prev: Vec::new(), temp: cpu_temp_file() })
}

pub fn cpu_info() -> CpuInfo {
    let ids = fs::read_to_string("/proc/stat").map(|t| stat_cpu_ids(&t)).unwrap_or_default();
    let model = fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| parse_cpuinfo(&t).0);
    CpuInfo { model, cores: ids.len(), groups: core_groups(&ids), temperature: cpu_temp_file().is_some() }
}

/// The file with the CPU package temperature in m°C: a CPU hwmon driver's
/// package sensor, else a CPU thermal zone.
fn cpu_temp_file() -> Option<PathBuf> {
    let mut best: Option<(u8, PathBuf)> = None;
    for dir in sorted_dirs(Path::new(HWMON)) {
        let Some(driver) = read(&dir, "name") else { continue };
        for input in sorted_dirs(&dir).into_iter().filter(|p| is_temp_input(p)) {
            let label_file = input.to_string_lossy().replace("_input", "_label");
            let label = fs::read_to_string(label_file).map(|l| l.trim().to_string()).unwrap_or_default();
            if let Some(rank) = hwmon_rank(&driver, &label)
                && best.as_ref().is_none_or(|(r, _)| rank < *r)
            {
                best = Some((rank, input));
            }
        }
    }
    if let Some((_, path)) = best {
        return Some(path);
    }
    sorted_dirs(Path::new(THERMAL))
        .into_iter()
        .find(|zone| read(zone, "type").is_some_and(|t| t == "x86_pkg_temp" || t.contains("cpu")))
        .map(|zone| zone.join("temp"))
}

fn is_temp_input(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("temp") && n.ends_with("_input"))
}

/// How well an hwmon sensor represents the whole CPU (lower is better);
/// `None` for sensors that are not the CPU's.
fn hwmon_rank(driver: &str, label: &str) -> Option<u8> {
    match driver {
        // Intel; AMD (k10temp, zenpower); Raspberry Pi and other ARM boards.
        "coretemp" | "k10temp" | "zenpower" | "cpu_thermal" | "cpu-thermal" => {}
        _ => return None,
    }
    Some(match label {
        "Package id 0" | "Tctl" | "Tdie" => 0,
        "" => 1,
        _ => 2,
    })
}

/// Entries of `dir`, sorted (hwmon numbering is not stable, but a sorted
/// scan at least picks the same sensor each time on one boot).
fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    paths
}

fn read_celsius(path: &Path) -> Option<f32> {
    let milli: i64 = fs::read_to_string(path).ok()?.trim().parse().ok()?;
    Some(milli as f32 / 1000.0)
}

/// P-cores and E-cores, as indices into the per-core loads (which follow
/// `ids`, the online CPUs).
fn core_groups(ids: &[u32]) -> Vec<CoreGroup> {
    // Intel hybrid CPUs register one perf PMU per core type.
    let list = |pmu: &str| fs::read_to_string(format!("/sys/devices/{pmu}/cpus")).ok().and_then(|t| parse_cpu_list(&t));
    let (p, e) = match (list("cpu_core"), list("cpu_atom")) {
        (Some(p), Some(e)) => (p, e),
        // ARM big.LITTLE: cores differ in relative capacity.
        _ => {
            let capacity = |id: &u32| read(Path::new(&format!("/sys/devices/system/cpu/cpu{id}")), "cpu_capacity")?.parse().ok();
            let Some(capacities) = ids.iter().map(capacity).collect::<Option<Vec<u32>>>() else { return Vec::new() };
            match split_by_capacity(ids, &capacities) {
                Some(split) => split,
                None => return Vec::new(),
            }
        }
    };
    let indices = |cpus: &[u32]| -> Vec<usize> { ids.iter().enumerate().filter(|(_, id)| cpus.contains(id)).map(|(i, _)| i).collect() };
    [("P-cores", indices(&p)), ("E-cores", indices(&e))]
        .into_iter()
        .filter(|(_, cores)| !cores.is_empty())
        .map(|(name, cores)| CoreGroup { name: name.into(), cores })
        .collect()
}

/// CPU ids split into (fast, slow) when there are exactly two capacities.
fn split_by_capacity(ids: &[u32], capacities: &[u32]) -> Option<(Vec<u32>, Vec<u32>)> {
    let mut distinct = capacities.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    let [_, high] = distinct[..] else { return None };
    let (fast, slow) = ids.iter().zip(capacities).partition::<Vec<_>, _>(|(_, c)| **c == high);
    Some((fast.into_iter().map(|(id, _)| *id).collect(), slow.into_iter().map(|(id, _)| *id).collect()))
}

/// Parses a kernel CPU list such as "0-15,20,22-23".
fn parse_cpu_list(text: &str) -> Option<Vec<u32>> {
    let mut ids = Vec::new();
    for part in text.trim().split(',').filter(|p| !p.is_empty()) {
        let (a, b) = part.split_once('-').unwrap_or((part, part));
        ids.extend(a.parse::<u32>().ok()?..=b.parse::<u32>().ok()?);
    }
    Some(ids)
}

/// Ids of the `cpuN` lines in /proc/stat, i.e. the online CPUs.
fn stat_cpu_ids(text: &str) -> Vec<u32> {
    text.lines().filter_map(|l| l.split_whitespace().next()?.strip_prefix("cpu")?.parse().ok()).collect()
}

pub use crate::nvml::gpus;

pub fn network(interface: Option<String>) -> Option<Box<dyn Source<sidebar_core::source::NetSample>>> {
    Some(crate::net_linux::network(interface))
}

pub fn memory() -> Box<dyn Source<Memory>> {
    Box::new(|| parse_meminfo(&fs::read_to_string("/proc/meminfo").ok()?))
}

pub fn battery() -> Option<Box<dyn Source<Battery>>> {
    let dirs = battery_dirs(Path::new(POWER_SUPPLY));
    if dirs.is_empty() {
        return None;
    }
    Some(Box::new(move || read_batteries(&dirs)))
}

pub fn utc_offset() -> Box<dyn Source<UtcOffset>> {
    Box::new(local_offset)
}

struct CpuSource {
    /// (busy, total) jiffies at the previous sample: the aggregate first,
    /// then each core.
    prev: Vec<(u64, u64)>,
    temp: Option<PathBuf>,
}

impl Source<CpuSample> for CpuSource {
    fn sample(&mut self) -> Option<CpuSample> {
        let now = parse_stat(&fs::read_to_string("/proc/stat").ok()?)?;
        let prev = std::mem::replace(&mut self.prev, now.clone());
        // First sample, or a core went on/offline: nothing to compare.
        if prev.len() != now.len() {
            return None;
        }
        let mut loads = now.iter().zip(&prev).map(|(n, p)| {
            let (busy, total) = (n.0.saturating_sub(p.0), n.1.saturating_sub(p.1));
            if total > 0 { busy as f32 / total as f32 } else { 0.0 }
        });
        let total = loads.next()?;
        let mhz = fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| parse_cpuinfo(&t).1);
        let celsius = self.temp.as_deref().and_then(read_celsius);
        Some(CpuSample { total, cores: loads.collect(), mhz, celsius })
    }
}

/// (busy, total) jiffies from /proc/stat: the aggregate `cpu` line first,
/// then each `cpuN` line.
fn parse_stat(text: &str) -> Option<Vec<(u64, u64)>> {
    let mut lines = text.lines().filter(|l| l.starts_with("cpu"));
    let first = lines.next().filter(|l| l.starts_with("cpu "))?;
    std::iter::once(first).chain(lines).map(parse_stat_line).collect()
}

fn parse_stat_line(line: &str) -> Option<(u64, u64)> {
    // user nice system idle iowait irq softirq steal [guest guest_nice];
    // guest time is already counted in user/nice.
    let fields: Vec<u64> = line.split_whitespace().skip(1).take(8).map(|f| f.parse().ok()).collect::<Option<_>>()?;
    if fields.len() < 4 {
        return None;
    }
    let total: u64 = fields.iter().sum();
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

/// (model name, average "cpu MHz") from /proc/cpuinfo. Either may be
/// missing (ARM kernels often report neither).
fn parse_cpuinfo(text: &str) -> (Option<String>, Option<f32>) {
    let model = cpuinfo_values(text, "model name").next().map(str::to_string);
    let mhz: Vec<f32> = cpuinfo_values(text, "cpu MHz").filter_map(|v| v.parse().ok()).collect();
    let mhz = (!mhz.is_empty()).then(|| mhz.iter().sum::<f32>() / mhz.len() as f32);
    (model, mhz)
}

fn cpuinfo_values<'a>(text: &'a str, key: &'a str) -> impl Iterator<Item = &'a str> {
    text.lines().filter_map(move |l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim())
    })
}

fn parse_meminfo(text: &str) -> Option<Memory> {
    let kib = |key: &str| {
        let line = text.lines().find(|l| l.starts_with(key) && l[key.len()..].starts_with(':'))?;
        line[key.len() + 1..].split_whitespace().next()?.parse::<u64>().ok()
    };
    let total = kib("MemTotal")?;
    let available = kib("MemAvailable")?;
    let (swap_total, swap_free) = (kib("SwapTotal").unwrap_or(0), kib("SwapFree").unwrap_or(0));
    Some(Memory {
        used_bytes: total.saturating_sub(available) * 1024,
        total_bytes: total * 1024,
        swap_used_bytes: swap_total.saturating_sub(swap_free) * 1024,
        swap_total_bytes: swap_total * 1024,
    })
}

fn read(dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(dir.join(name)).ok().map(|s| s.trim().to_string())
}

/// System batteries (not peripherals such as mice, which have
/// `scope = Device`).
fn battery_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|d| read(d, "type").as_deref() == Some("Battery") && read(d, "scope").as_deref() != Some("Device"))
        .collect();
    dirs.sort();
    dirs
}

/// One battery's raw readings.
#[derive(Debug, Default, PartialEq)]
struct Reading {
    /// (now, full) in µWh or µAh, when the driver reports them.
    charge: Option<(u64, u64)>,
    /// Percent, the fallback.
    capacity: Option<u64>,
    status: String,
}

fn read_battery(dir: &Path) -> Reading {
    let num = |name: &str| read(dir, name).and_then(|v| v.parse::<u64>().ok());
    let pair = |now, full| Some((num(now)?, num(full)?)).filter(|(_, full)| *full > 0);
    Reading {
        charge: pair("energy_now", "energy_full").or_else(|| pair("charge_now", "charge_full")),
        capacity: num("capacity"),
        status: read(dir, "status").unwrap_or_default(),
    }
}

fn read_batteries(dirs: &[PathBuf]) -> Option<Battery> {
    combine(&dirs.iter().map(|d| read_battery(d)).collect::<Vec<_>>())
}

/// Combines several batteries into one level, weighting by capacity when
/// every battery reports it.
fn combine(readings: &[Reading]) -> Option<Battery> {
    let level = if readings.iter().all(|r| r.charge.is_some()) {
        let (now, full) = readings.iter().filter_map(|r| r.charge).fold((0, 0), |a, c| (a.0 + c.0, a.1 + c.1));
        (full > 0).then(|| now as f32 / full as f32)?
    } else {
        let levels: Vec<f32> = readings.iter().filter_map(|r| r.capacity).map(|c| c as f32 / 100.0).collect();
        if levels.is_empty() {
            return None;
        }
        levels.iter().sum::<f32>() / levels.len() as f32
    };
    Some(Battery { level: level.clamp(0.0, 1.0), charging: readings.iter().any(|r| r.status == "Charging") })
}

unsafe extern "C" {
    /// POSIX; not exposed by the libc crate. glibc's localtime_r only loads
    /// the zone once unless this is called.
    fn tzset();
}

fn local_offset() -> Option<UtcOffset> {
    // SAFETY: time(NULL) has no preconditions; localtime_r writes only to
    // the `tm` we pass; tzset re-reads TZ / /etc/localtime so time zone
    // changes are picked up.
    unsafe {
        tzset();
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return None;
        }
        Some(UtcOffset(tm.tm_gmtoff as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_stat() {
        let text = "cpu  100 5 50 800 20 3 2 0 7 0\ncpu0 1 2 3 4 5 6 7 8 0 0\ncpu1 10 0 0 30 0 0 0 0\nintr 1 2\n";
        // total = 100+5+50+800+20+3+2+0 = 980, idle = 800+20.
        assert_eq!(parse_stat(text), Some(vec![(160, 980), (27, 36), (10, 40)]));
        assert_eq!(parse_stat("intr 1 2 3"), None);
        assert_eq!(parse_stat("cpu0 1 2 3 4\n"), None);
    }

    #[test]
    fn finds_core_types() {
        assert_eq!(parse_cpu_list("0-3,8,10-11\n"), Some(vec![0, 1, 2, 3, 8, 10, 11]));
        assert_eq!(parse_cpu_list("x"), None);
        assert_eq!(stat_cpu_ids("cpu  1 2\ncpu0 1 2\ncpu2 1 2\nintr 5\n"), [0, 2]);
        assert_eq!(split_by_capacity(&[0, 1, 2, 3], &[446, 446, 1024, 1024]), Some((vec![2, 3], vec![0, 1])));
        assert_eq!(split_by_capacity(&[0, 1], &[1024, 1024]), None);
        assert_eq!(split_by_capacity(&[0, 1, 2], &[100, 500, 1024]), None);
    }

    #[test]
    fn prefers_cpu_package_sensors() {
        assert_eq!(hwmon_rank("coretemp", "Package id 0"), Some(0));
        assert_eq!(hwmon_rank("k10temp", "Tctl"), Some(0));
        assert!(hwmon_rank("coretemp", "Core 4") > hwmon_rank("cpu_thermal", ""));
        assert_eq!(hwmon_rank("nvme", "Composite"), None);
        assert!(is_temp_input(Path::new("/x/temp12_input")));
        assert!(!is_temp_input(Path::new("/x/temp12_label")));
    }

    #[test]
    fn parses_cpuinfo() {
        let text = "processor\t: 0\nmodel name\t: Intel(R) Core(TM) i7\ncpu MHz\t\t: 1000.0\n\n\
                    processor\t: 1\nmodel name\t: Intel(R) Core(TM) i7\ncpu MHz\t\t: 3000.0\n";
        assert_eq!(parse_cpuinfo(text), (Some("Intel(R) Core(TM) i7".into()), Some(2000.0)));
        assert_eq!(parse_cpuinfo("processor : 0\n"), (None, None));
    }

    #[test]
    fn parses_meminfo() {
        let text = "MemTotal:       1000 kB\nMemFree:  10 kB\nMemAvailable:    600 kB\nSwapTotal: 200 kB\nSwapFree: 150 kB\n";
        let memory = Memory { used_bytes: 400 * 1024, total_bytes: 1000 * 1024, swap_used_bytes: 50 * 1024, swap_total_bytes: 200 * 1024 };
        assert_eq!(parse_meminfo(text), Some(memory));
        // No swap.
        let text = "MemTotal: 1000 kB\nMemAvailable: 600 kB\n";
        assert_eq!(parse_meminfo(text).map(|m| (m.swap_used_bytes, m.swap_total_bytes)), Some((0, 0)));
        assert_eq!(parse_meminfo("MemTotal: 5 kB\n"), None);
    }

    #[test]
    fn combines_batteries() {
        let r = |charge, capacity, status: &str| Reading { charge, capacity, status: status.into() };
        let both = [r(Some((30, 100)), Some(30), "Discharging"), r(Some((50, 50)), Some(100), "Charging")];
        assert_eq!(combine(&both), Some(Battery { level: 80.0 / 150.0, charging: true }));
        // Without energy readings, capacities are averaged.
        let caps = [r(None, Some(40), "Discharging"), r(Some((1, 2)), Some(60), "Full")];
        assert_eq!(combine(&caps), Some(Battery { level: 0.5, charging: false }));
        assert_eq!(combine(&[r(None, None, "Unknown")]), None);
    }

    #[test]
    fn reads_live_system() {
        assert!(memory().sample().is_some_and(|m| m.used_bytes <= m.total_bytes && m.total_bytes > 0));
        let mut cpu = cpu();
        assert!(cpu.sample().is_none(), "first sample has nothing to compare against");
        let cores = cpu_info().cores;
        assert!(cores > 0);
        assert!(cpu.sample().is_some_and(|s| s.cores.len() == cores));
        assert!(utc_offset().sample().is_some_and(|o| o.0.abs() <= 14 * 3600));
    }
}
