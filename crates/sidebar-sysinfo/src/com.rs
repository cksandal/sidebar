//! Calling COM interfaces by vtable index, for the few that windows-sys
//! leaves out (DXGI, Core Audio). Indices count IUnknown's three methods
//! and every inherited interface's methods first, as in the SDK headers.

use std::ffi::c_void;

/// Method `index` of a COM object's vtable.
///
/// # Safety
/// `this` must be a live COM object whose vtable has a method of type `F`
/// at `index`.
pub unsafe fn method<F: Copy>(this: *mut c_void, index: usize) -> F {
    // SAFETY: per the contract, a vtable pointer starts the object.
    unsafe {
        let vtable = *(this as *const *const *const c_void);
        std::mem::transmute_copy::<*const c_void, F>(&*vtable.add(index))
    }
}

/// IUnknown::Release.
///
/// # Safety
/// `this` must be a live COM object the caller holds a reference to.
pub unsafe fn release(this: *mut c_void) {
    // SAFETY: Release is method 2 of every COM interface.
    unsafe { method::<unsafe extern "system" fn(*mut c_void) -> u32>(this, 2)(this) };
}

/// Owns one COM reference and releases it when dropped.
pub struct Com(pub *mut c_void);

impl Drop for Com {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we hold this reference.
            unsafe { release(self.0) };
        }
    }
}
