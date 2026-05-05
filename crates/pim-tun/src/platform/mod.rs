#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::PlatformTunInterface;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::PlatformTunInterface;

// Phase B: android receives a TUN fd from the host
// `VpnService.establish()` call via the JNI bridge in
// `pim-daemon::jni`, which sets `PIM_TUN_FD`. The android backend
// adopts that fd; addressing/MTU/routes are configured by Java
// before `establish()` returns, so the native methods are no-ops.
#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub(crate) use android::PlatformTunInterface;

// Targets without a backend (windows, ios, freebsd, etc.) keep
// returning `TunError::Unavailable` from the unsupported stub.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "android")))]
mod unsupported;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "android")))]
pub(crate) use unsupported::PlatformTunInterface;
