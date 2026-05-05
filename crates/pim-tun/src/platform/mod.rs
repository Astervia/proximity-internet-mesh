#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::PlatformTunInterface;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::PlatformTunInterface;

// Android falls into the `unsupported` arm in Phase A: the real backend
// (driven by an FD passed in from `VpnService.establish()`) lands in
// Phase B. Until then the daemon honestly returns `TunError::Unavailable`
// at runtime, which keeps the workspace compile gate green for
// `aarch64-linux-android` without changing Linux/macOS behaviour.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) use unsupported::PlatformTunInterface;
