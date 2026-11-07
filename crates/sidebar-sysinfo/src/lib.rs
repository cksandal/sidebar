//! System data sources for widgets, one implementation per OS.
//!
//! Everything here returns `sidebar_core::Source`s, so widgets never see
//! which OS they run on. OS-specific code lives in one module per OS; on an
//! OS without one, sources report `None` and widgets show "n/a".

use sidebar_core::Source;
use sidebar_core::source::{Battery, CpuInfo, CpuSample, DiskSample, GpuInfo, GpuSample, Memory, NetSample, UtcOffset};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(target_os = "linux")]
mod net_linux;
#[cfg(target_os = "linux")]
mod audio_linux;
#[cfg(target_os = "linux")]
mod disk_linux;
#[cfg(target_os = "linux")]
mod drm_linux;
mod audio;
pub use audio::Audio;
#[cfg(any(target_os = "linux", windows))]
mod nvml;
#[cfg(any(target_os = "linux", windows))]
mod traffic;

#[cfg(windows)]
mod audio_windows;
#[cfg(windows)]
mod com;
#[cfg(windows)]
mod gpu_windows;
#[cfg(windows)]
mod net_windows;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(not(any(target_os = "linux", windows)))]
mod unsupported;
#[cfg(not(any(target_os = "linux", windows)))]
use unsupported as imp;

/// Total and per-core CPU load since the previous sample, and the clock.
pub fn cpu() -> Box<dyn Source<CpuSample>> {
    imp::cpu()
}

/// CPU model and core count.
pub fn cpu_info() -> CpuInfo {
    imp::cpu_info()
}

pub fn memory() -> Box<dyn Source<Memory>> {
    imp::memory()
}

/// `None` if the machine has no system battery (desktops).
pub fn battery() -> Option<Box<dyn Source<Battery>>> {
    imp::battery()
}

/// One GPU: what it is, and where its readings come from.
pub struct Gpu {
    pub info: GpuInfo,
    pub source: Box<dyn Source<GpuSample>>,
}

/// GPUs this build can read: NVIDIA through NVML, AMD through sysfs on
/// Linux, and the rest through performance counters on Windows. Empty
/// when there are none or the drivers are missing.
pub fn gpus() -> Vec<Gpu> {
    imp::gpus()
}

/// Traffic and addresses of `interface`, or of whichever interface has the
/// default route when `None`. `None` on OSes without an implementation.
pub fn network(interface: Option<String>) -> Option<Box<dyn Source<NetSample>>> {
    imp::network(interface)
}

/// Space on each mounted filesystem (Linux) or fixed drive (Windows), and
/// read and write rates across the physical disks.
pub fn disks() -> Box<dyn Source<DiskSample>> {
    imp::disks()
}

/// Volume and mute of the default audio output, with a control to change
/// them; `None` when there is no output or no way to reach it.
pub fn audio() -> Option<Audio> {
    imp::audio()
}

/// Local time zone offset; follows DST and time zone changes.
pub fn utc_offset() -> Box<dyn Source<UtcOffset>> {
    imp::utc_offset()
}
