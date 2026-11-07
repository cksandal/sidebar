//! NVIDIA GPUs through NVML, the driver's management library (what
//! `nvidia-smi` uses). It is loaded at run time, so the sidebar neither
//! links against nor requires the NVIDIA driver; without it there are
//! simply no NVIDIA GPUs.
//!
//! Only stable NVML entry points are used. The API is the same on Linux
//! (`libnvidia-ml.so.1`) and Windows (`nvml.dll`); only loading differs,
//! see [`dl`].

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::OnceLock;

use sidebar_core::Source;
use sidebar_core::source::{GpuInfo, GpuSample};

use crate::Gpu;

type Device = *mut c_void;
type Ret = c_int;
type GetU32 = unsafe extern "C" fn(Device, *mut c_uint) -> Ret;
type GetU32By = unsafe extern "C" fn(Device, c_int, *mut c_uint) -> Ret;
type GetSampled = unsafe extern "C" fn(Device, *mut c_uint, *mut c_uint) -> Ret;

const SUCCESS: Ret = 0;
const TEMPERATURE_GPU: c_int = 0;
const CLOCK_GRAPHICS: c_int = 0;
const CLOCK_MEM: c_int = 2;
/// NVML_DEVICE_NAME_V2_BUFFER_SIZE.
const NAME_LEN: usize = 96;

#[repr(C)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
struct MemoryInfo {
    total: u64,
    free: u64,
    used: u64,
}

/// NVML entry points. Only function pointers, so it can live in a static.
struct Api {
    device_count: unsafe extern "C" fn(*mut c_uint) -> Ret,
    device_by_index: unsafe extern "C" fn(c_uint, *mut Device) -> Ret,
    name: unsafe extern "C" fn(Device, *mut c_char, c_uint) -> Ret,
    utilization: unsafe extern "C" fn(Device, *mut Utilization) -> Ret,
    memory: unsafe extern "C" fn(Device, *mut MemoryInfo) -> Ret,
    temperature: GetU32By,
    clock: GetU32By,
    power: GetU32,
    power_limit: GetU32,
    fan: GetU32,
    encoder: GetSampled,
    decoder: GetSampled,
}

/// Loads and initializes NVML once per process. It is never unloaded:
/// device handles stay valid for as long as the sidebar runs.
fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(load).as_ref()
}

fn load() -> Option<Api> {
    // SAFETY: each symbol is cast to the signature documented in nvml.h
    // for that versioned name; nvmlInit_v2 is called before any other
    // entry point. The library is never unloaded.
    unsafe {
        let lib = dl::open()?;
        macro_rules! sym {
            ($name:literal) => {{
                let p = dl::sym(lib, $name)?;
                std::mem::transmute::<*mut c_void, _>(p)
            }};
        }
        let init: unsafe extern "C" fn() -> Ret = sym!(c"nvmlInit_v2");
        if init() != SUCCESS {
            return None;
        }
        Some(Api {
            device_count: sym!(c"nvmlDeviceGetCount_v2"),
            device_by_index: sym!(c"nvmlDeviceGetHandleByIndex_v2"),
            name: sym!(c"nvmlDeviceGetName"),
            utilization: sym!(c"nvmlDeviceGetUtilizationRates"),
            memory: sym!(c"nvmlDeviceGetMemoryInfo"),
            temperature: sym!(c"nvmlDeviceGetTemperature"),
            clock: sym!(c"nvmlDeviceGetClockInfo"),
            power: sym!(c"nvmlDeviceGetPowerUsage"),
            power_limit: sym!(c"nvmlDeviceGetEnforcedPowerLimit"),
            fan: sym!(c"nvmlDeviceGetFanSpeed"),
            encoder: sym!(c"nvmlDeviceGetEncoderUtilization"),
            decoder: sym!(c"nvmlDeviceGetDecoderUtilization"),
        })
    }
}

/// Finding the NVML library and its symbols.
#[cfg(unix)]
mod dl {
    use std::ffi::{CStr, c_void};

    pub type Lib = *mut c_void;

    pub unsafe fn open() -> Option<Lib> {
        // SAFETY: NUL-terminated name.
        let lib = unsafe { libc::dlopen(c"libnvidia-ml.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        (!lib.is_null()).then_some(lib)
    }

    pub unsafe fn sym(lib: Lib, name: &CStr) -> Option<*mut c_void> {
        // SAFETY: `lib` came from dlopen; NUL-terminated name.
        let p = unsafe { libc::dlsym(lib, name.as_ptr()) };
        (!p.is_null()).then_some(p)
    }
}

#[cfg(windows)]
mod dl {
    use std::ffi::{CStr, c_void};

    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};

    pub type Lib = HMODULE;

    pub unsafe fn open() -> Option<Lib> {
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        // Current drivers install nvml.dll in System32; only that folder is
        // searched, so a DLL next to the sidebar or in the working
        // directory is never picked up. Older drivers used the NVSMI folder.
        let program_files = std::env::var("ProgramW6432").or_else(|_| std::env::var("ProgramFiles")).ok();
        let nvsmi = program_files.map(|p| format!(r"{p}\NVIDIA Corporation\NVSMI\nvml.dll"));
        // SAFETY: NUL-terminated wide strings that outlive the calls.
        unsafe {
            let lib = LoadLibraryExW(wide("nvml.dll").as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
            if !lib.is_null() {
                return Some(lib);
            }
            let lib = LoadLibraryExW(wide(&nvsmi?).as_ptr(), std::ptr::null_mut(), 0);
            (!lib.is_null()).then_some(lib)
        }
    }

    pub unsafe fn sym(lib: Lib, name: &CStr) -> Option<*mut c_void> {
        // SAFETY: `lib` is a loaded module; NUL-terminated name.
        unsafe { GetProcAddress(lib, name.as_ptr().cast()) }.map(|f| f as *mut c_void)
    }
}

/// All NVIDIA GPUs, in NVML order (which follows PCI bus order).
pub fn gpus() -> Vec<Gpu> {
    let Some(api) = api() else { return Vec::new() };
    let mut count: c_uint = 0;
    // SAFETY: NVML is initialized; `count` is a valid out pointer.
    if unsafe { (api.device_count)(&mut count) } != SUCCESS {
        return Vec::new();
    }
    (0..count)
        .filter_map(|i| {
            let mut device: Device = std::ptr::null_mut();
            // SAFETY: as above; `i` < count.
            if unsafe { (api.device_by_index)(i, &mut device) } != SUCCESS {
                return None;
            }
            let source = NvmlSource { api, device };
            let info = GpuInfo { name: source.name().unwrap_or_else(|| "NVIDIA GPU".into()), temperature: source.celsius().is_some() };
            Some(Gpu { info, source: Box::new(source) })
        })
        .collect()
}

struct NvmlSource {
    api: &'static Api,
    device: Device,
}

impl NvmlSource {
    fn name(&self) -> Option<String> {
        let mut buf = [0 as c_char; NAME_LEN];
        // SAFETY: the buffer is NAME_LEN bytes and NVML NUL-terminates.
        let ok = unsafe { (self.api.name)(self.device, buf.as_mut_ptr(), NAME_LEN as c_uint) } == SUCCESS;
        // SAFETY: NUL-terminated by NVML on success.
        ok.then(|| unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
    }

    fn get(&self, f: GetU32) -> Option<u32> {
        let mut v: c_uint = 0;
        // SAFETY: valid device handle and out pointer.
        (unsafe { f(self.device, &mut v) } == SUCCESS).then_some(v)
    }

    fn get_by(&self, f: GetU32By, kind: c_int) -> Option<u32> {
        let mut v: c_uint = 0;
        // SAFETY: valid device handle, enum value from nvml.h, out pointer.
        (unsafe { f(self.device, kind, &mut v) } == SUCCESS).then_some(v)
    }

    fn sampled(&self, f: GetSampled) -> Option<f32> {
        let (mut v, mut period): (c_uint, c_uint) = (0, 0);
        // SAFETY: valid device handle and out pointers.
        (unsafe { f(self.device, &mut v, &mut period) } == SUCCESS).then(|| v as f32 / 100.0)
    }

    fn celsius(&self) -> Option<f32> {
        self.get_by(self.api.temperature, TEMPERATURE_GPU).map(|c| c as f32)
    }
}

impl Source<GpuSample> for NvmlSource {
    fn sample(&mut self) -> Option<GpuSample> {
        let api = self.api;
        let mut util = Utilization { gpu: 0, memory: 0 };
        let mut mem = MemoryInfo { total: 0, free: 0, used: 0 };
        // SAFETY: valid device handle and out pointers.
        unsafe {
            if (api.utilization)(self.device, &mut util) != SUCCESS || (api.memory)(self.device, &mut mem) != SUCCESS {
                return None;
            }
        }
        let watts = |mw: u32| mw as f32 / 1000.0;
        Some(GpuSample {
            load: util.gpu as f32 / 100.0,
            memory_load: Some(util.memory as f32 / 100.0),
            memory_used_bytes: mem.used,
            memory_total_bytes: mem.total,
            encoder_load: self.sampled(api.encoder),
            decoder_load: self.sampled(api.decoder),
            fan: self.get(api.fan).map(|f| f as f32 / 100.0),
            power_watts: self.get(api.power).map(watts),
            power_limit_watts: self.get(api.power_limit).map(watts),
            core_mhz: self.get_by(api.clock, CLOCK_GRAPHICS).map(|m| m as f32),
            memory_mhz: self.get_by(api.clock, CLOCK_MEM).map(|m| m as f32),
            celsius: self.celsius(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs only where an NVIDIA driver is installed.
    #[test]
    fn reads_live_gpus() {
        for mut gpu in gpus() {
            assert!(!gpu.info.name.is_empty());
            let s = gpu.source.sample().expect("sample");
            assert!(s.memory_total_bytes > 0 && s.memory_used_bytes <= s.memory_total_bytes);
            assert!((0.0..=1.0).contains(&s.load));
        }
    }
}
