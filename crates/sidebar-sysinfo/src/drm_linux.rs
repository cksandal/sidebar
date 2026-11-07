//! AMD GPUs through the amdgpu driver's sysfs files: load, VRAM, clocks,
//! temperature, power and fan, with the model name from the PCI ID
//! database. (Intel's drivers only report GPU load through perf events,
//! which need extra privileges, so Intel GPUs are not listed.)

use std::fs;
use std::path::{Path, PathBuf};

use sidebar_core::Source;
use sidebar_core::source::{GpuInfo, GpuSample};

use crate::Gpu;

const AMD: u32 = 0x1002;

/// AMD GPUs, in card order.
pub fn gpus() -> Vec<Gpu> {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else { return Vec::new() };
    let mut cards: Vec<(u32, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let n = name.strip_prefix("card")?.parse().ok()?;
            Some((n, e.path().join("device")))
        })
        .collect();
    cards.sort();
    let ids = ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids"].iter().find_map(|p| fs::read_to_string(p).ok());
    cards.into_iter().filter_map(|(_, device)| amd_gpu(&device, ids.as_deref())).collect()
}

fn amd_gpu(device: &Path, ids: Option<&str>) -> Option<Gpu> {
    let hex = |file: &str| u32::from_str_radix(fs::read_to_string(device.join(file)).ok()?.trim().trim_start_matches("0x"), 16).ok();
    if hex("vendor")? != AMD || !device.join("gpu_busy_percent").exists() {
        return None;
    }
    let name = fs::read_to_string(device.join("product_name"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| pci_name(ids?, AMD, hex("device")?))
        .unwrap_or_else(|| "AMD GPU".into());
    let source = AmdSource { device: device.to_path_buf(), hwmon: hwmon(device) };
    let info = GpuInfo { name, temperature: source.celsius().is_some() };
    Some(Gpu { info, source: Box::new(source) })
}

fn hwmon(device: &Path) -> Option<PathBuf> {
    fs::read_dir(device.join("hwmon")).ok()?.flatten().map(|e| e.path()).next()
}

/// "AMD Radeon RX 6800/6800 XT / 6900 XT" from the PCI ID database: the
/// marketing name in brackets if there is one, else the chip name.
fn pci_name(ids: &str, vendor: u32, device: u32) -> Option<String> {
    let vendor = format!("{vendor:04x}");
    let device = format!("\t{device:04x}");
    let mut in_vendor = false;
    for line in ids.lines() {
        if !line.starts_with('\t') && !line.starts_with('#') && !line.is_empty() {
            in_vendor = line.starts_with(&vendor);
        } else if in_vendor && line.starts_with(&device) {
            let name = line[device.len()..].trim();
            let name = match (name.find('['), name.rfind(']')) {
                (Some(a), Some(b)) if a < b => &name[a + 1..b],
                _ => name,
            };
            return Some(format!("AMD {name}"));
        }
    }
    None
}

struct AmdSource {
    device: PathBuf,
    hwmon: Option<PathBuf>,
}

fn read_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

impl AmdSource {
    fn device(&self, file: &str) -> Option<u64> {
        read_u64(&self.device.join(file))
    }

    fn hwmon(&self, file: &str) -> Option<u64> {
        read_u64(&self.hwmon.as_ref()?.join(file))
    }

    fn celsius(&self) -> Option<f32> {
        self.hwmon("temp1_input").map(|m| m as f32 / 1000.0)
    }
}

impl Source<GpuSample> for AmdSource {
    fn sample(&mut self) -> Option<GpuSample> {
        let percent = |v: u64| (v as f32 / 100.0).clamp(0.0, 1.0);
        let watts = |file: &str| self.hwmon(file).map(|uw| uw as f32 / 1e6);
        let mhz = |file: &str| self.hwmon(file).map(|hz| hz as f32 / 1e6);
        Some(GpuSample {
            load: percent(self.device("gpu_busy_percent")?),
            memory_load: self.device("mem_busy_percent").map(percent),
            memory_used_bytes: self.device("mem_info_vram_used").unwrap_or(0),
            memory_total_bytes: self.device("mem_info_vram_total").unwrap_or(0),
            // Not in sysfs.
            encoder_load: None,
            decoder_load: None,
            fan: self.hwmon("pwm1").map(|p| p as f32 / 255.0),
            power_watts: watts("power1_average").or_else(|| watts("power1_input")),
            power_limit_watts: watts("power1_cap"),
            core_mhz: mhz("freq1_input"),
            memory_mhz: mhz("freq2_input"),
            celsius: self.celsius(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_pci_ids() {
        let ids = "\
# comment
1000  Broadcom
\t0001  Something
1002  Advanced Micro Devices, Inc. [AMD/ATI]
\t73bf  Navi 21 [Radeon RX 6800/6800 XT / 6900 XT]
\t\t1002 0e3a  Radeon RX 6900 XT
\t1638  Cezanne [Radeon Vega Series / Radeon Vega Mobile Series]
\t7480  Navi 33
";
        assert_eq!(pci_name(ids, AMD, 0x73bf).as_deref(), Some("AMD Radeon RX 6800/6800 XT / 6900 XT"));
        assert_eq!(pci_name(ids, AMD, 0x7480).as_deref(), Some("AMD Navi 33"));
        assert_eq!(pci_name(ids, AMD, 0x0001), None);
    }

    #[test]
    fn reads_amdgpu_sysfs() {
        let dir = std::env::temp_dir().join(format!("sidebar-amdgpu-{}", std::process::id()));
        let device = dir.join("device");
        let hwmon = device.join("hwmon/hwmon3");
        fs::create_dir_all(&hwmon).unwrap();
        for (file, value) in [
            ("vendor", "0x1002"),
            ("device", "0x73bf"),
            ("product_name", "AMD Radeon RX 6800"),
            ("gpu_busy_percent", "37"),
            ("mem_busy_percent", "5"),
            ("mem_info_vram_used", "1073741824"),
            ("mem_info_vram_total", "17163091968"),
        ] {
            fs::write(device.join(file), format!("{value}\n")).unwrap();
        }
        for (file, value) in [("temp1_input", "51000"), ("power1_average", "45000000"), ("power1_cap", "203000000"), ("pwm1", "51"), ("freq1_input", "2100000000")] {
            fs::write(hwmon.join(file), format!("{value}\n")).unwrap();
        }
        let mut gpu = amd_gpu(&device, None).expect("an AMD GPU");
        assert_eq!(gpu.info, GpuInfo { name: "AMD Radeon RX 6800".into(), temperature: true });
        let s = gpu.source.sample().unwrap();
        assert_eq!((s.load, s.memory_load, s.memory_used_bytes), (0.37, Some(0.05), 1 << 30));
        assert_eq!((s.celsius, s.power_watts, s.power_limit_watts, s.fan, s.core_mhz), (Some(51.0), Some(45.0), Some(203.0), Some(0.2), Some(2100.0)));
        fs::write(device.join("vendor"), "0x10de\n").unwrap();
        assert!(amd_gpu(&device, None).is_none());
        fs::remove_dir_all(&dir).unwrap();
    }
}
