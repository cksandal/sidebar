//! Fallback for operating systems without an implementation.

use sidebar_core::Source;
use sidebar_core::source::{Battery, CpuInfo, CpuSample, Memory, UtcOffset};

pub fn cpu() -> Box<dyn Source<CpuSample>> {
    Box::new(|| None)
}

pub fn cpu_info() -> CpuInfo {
    CpuInfo::default()
}

pub fn memory() -> Box<dyn Source<Memory>> {
    Box::new(|| None)
}

pub fn gpus() -> Vec<crate::Gpu> {
    Vec::new()
}

pub fn network(_interface: Option<String>) -> Option<Box<dyn Source<sidebar_core::source::NetSample>>> {
    None
}

pub fn battery() -> Option<Box<dyn Source<Battery>>> {
    None
}

pub fn utc_offset() -> Box<dyn Source<UtcOffset>> {
    Box::new(|| Some(UtcOffset(0)))
}
