//! C ABI used by the iOS Packet Tunnel Provider extension to drive the PIM
//! core. The public surface is:
//!
//! ```text
//! const char *pim_ffi_version(void);
//! PimHandle *pim_ffi_start(const char *config_json, char **err_out);
//! void       pim_ffi_stop(PimHandle *handle);
//! void       pim_ffi_free_string(char *s);
//! ```
//!
//! Plan 1 (issue #70, milestone 1) ships only the lifecycle scaffolding —
//! `pim_ffi_start` allocates an opaque handle and validates the config JSON
//! shape, and `pim_ffi_stop` releases it. Driving the daemon runtime,
//! bridging `NEPacketTunnelFlow.readPackets`/`writePackets`, and wiring
//! routing/discovery on iOS all land in follow-up plans. See
//! `docs/architecture/ios.md` for the full picture.

#![warn(missing_docs)]

use std::ffi::{c_char, CStr, CString};
use std::ptr;

/// Opaque handle returned by [`pim_ffi_start`]. Callers must treat the
/// pointer as a black box and pass it back to [`pim_ffi_stop`] exactly once.
pub struct PimHandle {
    // Placeholder — Plan 2 replaces this with a tokio runtime and a
    // DaemonState. Kept non-empty so every handle has distinct identity.
    _private: (),
}

/// Returns a NUL-terminated UTF-8 string owned by the library describing
/// the crate version. The caller must not free the returned pointer.
#[no_mangle]
pub extern "C" fn pim_ffi_version() -> *const c_char {
    // The byte string lives for the lifetime of the library, so the pointer
    // is always valid.
    const VERSION: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();
    VERSION.as_ptr() as *const c_char
}

/// Start the PIM runtime with the given JSON config.
///
/// On success returns a non-null `*mut PimHandle` that the caller must
/// release with [`pim_ffi_stop`]. On failure returns null and, if `err_out`
/// is non-null, writes a library-owned NUL-terminated UTF-8 error string
/// to `*err_out` that the caller must release with [`pim_ffi_free_string`].
///
/// Plan 1 only validates that `config_json` parses as a JSON value — it does
/// not yet start the daemon. Plan 2 will feed the parsed config into
/// `pim_core::Config`.
///
/// # Safety
///
/// - `config_json` must be a NUL-terminated UTF-8 C string pointing at a
///   valid, readable buffer.
/// - `err_out` may be null. If non-null, it must point at a writable
///   `*mut c_char` slot.
#[no_mangle]
pub unsafe extern "C" fn pim_ffi_start(
    config_json: *const c_char,
    err_out: *mut *mut c_char,
) -> *mut PimHandle {
    if config_json.is_null() {
        set_error(err_out, "config_json is null");
        return ptr::null_mut();
    }

    let config_cstr = CStr::from_ptr(config_json);
    let config_str = match config_cstr.to_str() {
        Ok(s) => s,
        Err(_) => {
            set_error(err_out, "config_json is not valid UTF-8");
            return ptr::null_mut();
        }
    };

    // Shape-check the config JSON. Plan 2 will validate against pim_core::Config.
    if let Err(e) = serde_json::from_str::<serde_json::Value>(config_str) {
        set_error(err_out, &format!("invalid config JSON: {e}"));
        return ptr::null_mut();
    }

    Box::into_raw(Box::new(PimHandle { _private: () }))
}

/// Release a handle returned by [`pim_ffi_start`]. Passing null is a no-op;
/// passing the same non-null handle twice is undefined.
///
/// # Safety
///
/// `handle` must be either null or a pointer returned by
/// [`pim_ffi_start`] that has not yet been passed to `pim_ffi_stop`.
#[no_mangle]
pub unsafe extern "C" fn pim_ffi_stop(handle: *mut PimHandle) {
    if handle.is_null() {
        return;
    }
    drop(Box::from_raw(handle));
}

/// Free an error string previously written by [`pim_ffi_start`]. Null is a
/// no-op.
///
/// # Safety
///
/// `s` must be either null or a pointer previously written to
/// `*err_out` by [`pim_ffi_start`].
#[no_mangle]
pub unsafe extern "C" fn pim_ffi_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    drop(CString::from_raw(s));
}

unsafe fn set_error(err_out: *mut *mut c_char, msg: &str) {
    if err_out.is_null() {
        return;
    }
    let c = match CString::new(msg) {
        Ok(c) => c,
        // The only way CString::new fails is an interior NUL in `msg`.
        // We never pass one, but if we ever did we silently drop the
        // message rather than writing a half-formed pointer.
        Err(_) => return,
    };
    *err_out = c.into_raw();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn version_is_non_empty() {
        let p = pim_ffi_version();
        assert!(!p.is_null());
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap();
        assert!(!s.is_empty(), "version string should be non-empty");
    }

    #[test]
    fn start_and_stop_round_trip() {
        let cfg = CString::new(r#"{"node":{"name":"test"}}"#).unwrap();
        let mut err: *mut c_char = ptr::null_mut();
        let handle = unsafe { pim_ffi_start(cfg.as_ptr(), &mut err as *mut _) };
        assert!(!handle.is_null(), "expected non-null handle, got err");
        assert!(err.is_null(), "did not expect an error string");
        unsafe { pim_ffi_stop(handle) };
    }

    #[test]
    fn start_rejects_invalid_json() {
        let cfg = CString::new("not valid json {{{").unwrap();
        let mut err: *mut c_char = ptr::null_mut();
        let handle = unsafe { pim_ffi_start(cfg.as_ptr(), &mut err as *mut _) };
        assert!(handle.is_null());
        assert!(!err.is_null());
        let msg = unsafe { CStr::from_ptr(err) }.to_str().unwrap().to_string();
        assert!(msg.contains("invalid config JSON"), "got error {msg:?}");
        unsafe { pim_ffi_free_string(err) };
    }

    #[test]
    fn start_rejects_null_config() {
        let mut err: *mut c_char = ptr::null_mut();
        let handle = unsafe { pim_ffi_start(ptr::null(), &mut err as *mut _) };
        assert!(handle.is_null());
        assert!(!err.is_null());
        unsafe { pim_ffi_free_string(err) };
    }

    #[test]
    fn stop_on_null_is_safe() {
        unsafe { pim_ffi_stop(ptr::null_mut()) };
    }

    #[test]
    fn free_string_on_null_is_safe() {
        unsafe { pim_ffi_free_string(ptr::null_mut()) };
    }
}
