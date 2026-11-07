//! Audio through Core Audio: the default render endpoint's
//! IAudioEndpointVolume, looked up again on every read so a change of
//! output device (headphones plugged in) is followed.

#![allow(non_upper_case_globals)]

use std::ffi::c_void;

use sidebar_core::source::AudioSample;
use windows_sys::core::GUID;

use crate::audio::{self, Audio, Backend};
use crate::com::{Com, method};

windows_link::link!("ole32.dll" "system" fn CoInitializeEx(reserved: *const c_void, coinit: u32) -> i32);
windows_link::link!("ole32.dll" "system" fn CoCreateInstance(clsid: *const GUID, outer: *mut c_void, context: u32, iid: *const GUID, out: *mut *mut c_void) -> i32);
windows_link::link!("ole32.dll" "system" fn PropVariantClear(value: *mut PropVariant) -> i32);

const CLSID_MMDeviceEnumerator: GUID = GUID::from_u128(0xbcde0395_e52f_467c_8e3d_c4579291692e);
const IID_IMMDeviceEnumerator: GUID = GUID::from_u128(0xa95664d2_9614_4f35_a746_de8db63617e6);
const IID_IAudioEndpointVolume: GUID = GUID::from_u128(0x5cdf2c82_841e_4546_9722_0cf74078229a);
const CLSCTX_ALL: u32 = 0x17;
const COINIT_MULTITHREADED: u32 = 0;
/// EDataFlow eRender.
const RENDER: u32 = 0;
/// ERole eMultimedia.
const MULTIMEDIA: u32 = 1;
const STGM_READ: u32 = 0;
const VT_LPWSTR: u16 = 31;

#[repr(C)]
struct PropertyKey {
    fmtid: GUID,
    pid: u32,
}

/// PKEY_Device_FriendlyName: "Speakers (Realtek(R) Audio)".
const FRIENDLY_NAME: PropertyKey = PropertyKey { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };

/// PROPVARIANT, as far as a string value needs.
#[repr(C)]
pub struct PropVariant {
    vt: u16,
    reserved: [u16; 3],
    pointer: *mut u16,
    rest: usize,
}

type Hr = i32;
type This = *mut c_void;

pub fn audio() -> Option<Audio> {
    audio::spawn("audio", || {
        // SAFETY: once on this thread, which keeps COM until it ends.
        unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED) };
        let mut enumerator = std::ptr::null_mut();
        // SAFETY: valid GUIDs and out pointer.
        let hr = unsafe { CoCreateInstance(&CLSID_MMDeviceEnumerator, std::ptr::null_mut(), CLSCTX_ALL, &IID_IMMDeviceEnumerator, &mut enumerator) };
        (hr >= 0).then(|| CoreAudio { enumerator: Com(enumerator) })
    })
}

struct CoreAudio {
    enumerator: Com,
}

impl CoreAudio {
    /// The default output device.
    fn device(&self) -> Option<Com> {
        let mut device = std::ptr::null_mut();
        // SAFETY: IMMDeviceEnumerator::GetDefaultAudioEndpoint is method 4.
        let hr = unsafe { method::<unsafe extern "system" fn(This, u32, u32, *mut This) -> Hr>(self.enumerator.0, 4)(self.enumerator.0, RENDER, MULTIMEDIA, &mut device) };
        (hr >= 0).then_some(Com(device))
    }

    /// The default device's volume control.
    fn volume(&self) -> Option<(Com, Com)> {
        let device = self.device()?;
        let mut volume = std::ptr::null_mut();
        // SAFETY: IMMDevice::Activate is method 3.
        let hr = unsafe {
            method::<unsafe extern "system" fn(This, *const GUID, u32, *mut c_void, *mut This) -> Hr>(device.0, 3)(
                device.0,
                &IID_IAudioEndpointVolume,
                CLSCTX_ALL,
                std::ptr::null_mut(),
                &mut volume,
            )
        };
        (hr >= 0).then_some((device, Com(volume)))
    }

    fn name(device: &Com) -> Option<String> {
        let mut store = std::ptr::null_mut();
        // SAFETY: IMMDevice::OpenPropertyStore is method 4;
        // IPropertyStore::GetValue is method 5. The value is cleared after
        // its string is copied.
        unsafe {
            if method::<unsafe extern "system" fn(This, u32, *mut This) -> Hr>(device.0, 4)(device.0, STGM_READ, &mut store) < 0 {
                return None;
            }
            let store = Com(store);
            let mut value = PropVariant { vt: 0, reserved: [0; 3], pointer: std::ptr::null_mut(), rest: 0 };
            if method::<unsafe extern "system" fn(This, *const PropertyKey, *mut PropVariant) -> Hr>(store.0, 5)(store.0, &FRIENDLY_NAME, &mut value) < 0 {
                return None;
            }
            let name = (value.vt == VT_LPWSTR && !value.pointer.is_null()).then(|| {
                let len = (0..).take_while(|&i| *value.pointer.add(i) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(value.pointer, len))
            });
            PropVariantClear(&mut value);
            name
        }
    }
}

impl Backend for CoreAudio {
    fn read(&mut self) -> Option<AudioSample> {
        let (device, volume) = self.volume()?;
        let (mut level, mut muted) = (0f32, 0i32);
        // SAFETY: IAudioEndpointVolume::GetMasterVolumeLevelScalar is
        // method 9 and GetMute method 15.
        unsafe {
            if method::<unsafe extern "system" fn(This, *mut f32) -> Hr>(volume.0, 9)(volume.0, &mut level) < 0 {
                return None;
            }
            method::<unsafe extern "system" fn(This, *mut i32) -> Hr>(volume.0, 15)(volume.0, &mut muted);
        }
        Some(AudioSample { volume: level, muted: muted != 0, device: Self::name(&device) })
    }

    fn set_volume(&mut self, level: f32) {
        if let Some((_, volume)) = self.volume() {
            // SAFETY: SetMasterVolumeLevelScalar is method 7.
            unsafe { method::<unsafe extern "system" fn(This, f32, *const GUID) -> Hr>(volume.0, 7)(volume.0, level, std::ptr::null()) };
        }
    }

    fn set_muted(&mut self, muted: bool) {
        if let Some((_, volume)) = self.volume() {
            // SAFETY: SetMute is method 14.
            unsafe { method::<unsafe extern "system" fn(This, i32, *const GUID) -> Hr>(volume.0, 14)(volume.0, muted.into(), std::ptr::null()) };
        }
    }
}
