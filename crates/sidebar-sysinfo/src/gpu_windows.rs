//! GPUs other than NVIDIA's (or all of them when NVML is missing) through
//! DXGI, for names and memory size, and the "GPU Engine" and "GPU Adapter
//! Memory" performance counters, the same numbers Task Manager shows.
//! Works for any vendor with a WDDM driver; there is no temperature.

#![allow(non_snake_case, non_camel_case_types)]

use std::ffi::c_void;

use sidebar_core::Source;
use sidebar_core::source::{GpuInfo, GpuSample};
use windows_sys::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows_sys::core::GUID;

use crate::Gpu;
use crate::com::{method, release};
use crate::windows::{from_wide, wide};

const NVIDIA: u32 = 0x10de;
/// Microsoft Basic Render Driver.
const MICROSOFT: u32 = 0x1414;
const DXGI_ADAPTER_FLAG_SOFTWARE: u32 = 2;
windows_link::link!("dxgi.dll" "system" fn CreateDXGIFactory1(riid: *const GUID, factory: *mut *mut c_void) -> i32);

/// DXGI_ADAPTER_DESC1, from dxgi.h.
#[repr(C)]
struct DXGI_ADAPTER_DESC1 {
    Description: [u16; 128],
    VendorId: u32,
    DeviceId: u32,
    SubSysId: u32,
    Revision: u32,
    DedicatedVideoMemory: usize,
    DedicatedSystemMemory: usize,
    SharedSystemMemory: usize,
    AdapterLuid: windows_sys::Win32::Foundation::LUID,
    Flags: u32,
}

const IID_IDXGI_FACTORY1: GUID = GUID::from_u128(0x770aae78_f26f_4dba_a829_253c83d1b387);

/// Hardware adapters, in DXGI order; NVIDIA's only if `nvidia`.
fn adapters(nvidia: bool) -> Vec<DXGI_ADAPTER_DESC1> {
    let mut factory: *mut c_void = std::ptr::null_mut();
    // SAFETY: a valid IID and out pointer.
    if unsafe { CreateDXGIFactory1(&IID_IDXGI_FACTORY1, &mut factory) } < 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0.. {
        let mut adapter: *mut c_void = std::ptr::null_mut();
        // SAFETY: IDXGIFactory1::EnumAdapters1 is method 12; fails (with
        // DXGI_ERROR_NOT_FOUND) past the last adapter.
        let hr = unsafe { method::<unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32>(factory, 12)(factory, i, &mut adapter) };
        if hr < 0 {
            break;
        }
        // SAFETY: zeroed is a valid DXGI_ADAPTER_DESC1; IDXGIAdapter1::
        // GetDesc1 is method 10.
        let mut desc: DXGI_ADAPTER_DESC1 = unsafe { std::mem::zeroed() };
        let ok = unsafe { method::<unsafe extern "system" fn(*mut c_void, *mut DXGI_ADAPTER_DESC1) -> i32>(adapter, 10)(adapter, &mut desc) } >= 0;
        // SAFETY: we own this reference.
        unsafe { release(adapter) };
        let hardware = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE == 0 && desc.VendorId != MICROSOFT;
        if ok && hardware && (nvidia || desc.VendorId != NVIDIA) {
            out.push(desc);
        }
    }
    // SAFETY: as above.
    unsafe { release(factory) };
    out
}

/// GPUs not already covered by NVML (`nvml_found`).
pub fn gpus(nvml_found: bool) -> Vec<Gpu> {
    adapters(!nvml_found)
        .into_iter()
        .filter_map(|desc| {
            let luid = luid_tag(desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart);
            let engines = Counters::open(r"\GPU Engine(*)\Utilization Percentage")?;
            let memory = Counters::open(r"\GPU Adapter Memory(*)\Dedicated Usage");
            let name = from_wide(&desc.Description);
            let source = PdhGpu { luid, engines, memory, total: desc.DedicatedVideoMemory as u64 };
            Some(Gpu { info: GpuInfo { name, temperature: false }, source: Box::new(source) })
        })
        .collect()
}

/// How counter instances name an adapter: "luid_0x00000000_0x0000D1F2".
fn luid_tag(high: u32, low: u32) -> String {
    format!("luid_0x{high:08x}_0x{low:08x}")
}

struct PdhGpu {
    luid: String,
    engines: Counters,
    memory: Option<Counters>,
    total: u64,
}

impl Source<GpuSample> for PdhGpu {
    fn sample(&mut self) -> Option<GpuSample> {
        let engines = self.engines.read()?;
        let loads = engine_loads(&engines, &self.luid);
        let used = self.memory.as_mut().and_then(Counters::read).and_then(|m| {
            m.iter().filter(|(name, _)| name.to_ascii_lowercase().starts_with(&self.luid)).map(|(_, v)| *v as u64).max()
        });
        Some(GpuSample {
            load: loads.busiest,
            memory_used_bytes: used.unwrap_or(0),
            memory_total_bytes: self.total,
            encoder_load: loads.encode,
            decoder_load: loads.decode,
            ..GpuSample::default()
        })
    }
}

#[derive(Debug, PartialEq)]
struct EngineLoads {
    /// The busiest engine, as Task Manager's overall figure.
    busiest: f32,
    encode: Option<f32>,
    decode: Option<f32>,
}

/// Per-engine loads of the adapter `luid` from "GPU Engine" instances
/// ("pid_1234_luid_0x…_0x…_phys_0_eng_3_engtype_VideoDecode"), summed over
/// processes, as fractions.
fn engine_loads(instances: &[(String, f64)], luid: &str) -> EngineLoads {
    let mut engines: Vec<(String, f64)> = Vec::new();
    for (name, value) in instances {
        let name = name.to_ascii_lowercase();
        let Some(at) = name.find(luid) else { continue };
        let engine = &name[at..];
        match engines.iter_mut().find(|(e, _)| e == engine) {
            Some((_, sum)) => *sum += value,
            None => engines.push((engine.to_string(), *value)),
        }
    }
    let fraction = |v: f64| (v / 100.0).clamp(0.0, 1.0) as f32;
    let of_type = |kind: &str| {
        engines.iter().filter(|(e, _)| e.ends_with(&format!("engtype_{kind}"))).map(|(_, v)| fraction(*v)).reduce(f32::max)
    };
    EngineLoads {
        busiest: engines.iter().map(|(_, v)| fraction(*v)).fold(0.0, f32::max),
        encode: of_type("videoencode"),
        decode: of_type("videodecode"),
    }
}

/// A wildcard PDH counter: one value per instance.
struct Counters {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

impl Counters {
    fn open(path: &str) -> Option<Self> {
        let path = wide(path);
        let (mut query, mut counter) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: valid out pointers; English counter names work in any
        // display language.
        unsafe {
            if PdhOpenQueryW(std::ptr::null(), 0, &mut query) != 0 {
                return None;
            }
            if PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) != 0 {
                PdhCloseQuery(query);
                return None;
            }
            PdhCollectQueryData(query);
        }
        Some(Self { query, counter })
    }

    /// (instance, value) pairs from a fresh collection.
    fn read(&mut self) -> Option<Vec<(String, f64)>> {
        // SAFETY: handles from `open`; the buffer is sized as PDH asks and
        // aligned for the items, whose names point into it.
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            let (mut bytes, mut count) = (0u32, 0u32);
            if PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, std::ptr::null_mut()) != PDH_MORE_DATA {
                return Some(Vec::new());
            }
            let mut buf = vec![0u64; (bytes as usize).div_ceil(8)];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, items) != 0 {
                return None;
            }
            let items = std::slice::from_raw_parts(items, count as usize);
            Some(items.iter().map(|item| (wide_ptr(item.szName), item.FmtValue.Anonymous.doubleValue)).collect())
        }
    }
}

/// A NUL-terminated UTF-16 string.
unsafe fn wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let len = (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count();
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

impl Drop for Counters {
    fn drop(&mut self) {
        // SAFETY: open until here.
        unsafe { PdhCloseQuery(self.query) };
    }
}

// The query is only used from the thread that owns the source.
unsafe impl Send for Counters {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_engines_over_processes() {
        let luid = luid_tag(0, 0xD1F2);
        let i = |s: &str, v| (s.to_string(), v);
        let instances = [
            i("pid_10_luid_0x00000000_0x0000D1F2_phys_0_eng_0_engtype_3D", 20.0),
            i("pid_11_luid_0x00000000_0x0000D1F2_phys_0_eng_0_engtype_3D", 15.0),
            i("pid_11_luid_0x00000000_0x0000D1F2_phys_0_eng_5_engtype_VideoDecode", 40.0),
            i("pid_12_luid_0x00000000_0x0000AAAA_phys_0_eng_0_engtype_3D", 90.0),
        ];
        assert_eq!(engine_loads(&instances, &luid), EngineLoads { busiest: 0.4, encode: None, decode: Some(0.4) });
        assert_eq!(engine_loads(&instances[..2], &luid).busiest, 0.35);
    }
}
