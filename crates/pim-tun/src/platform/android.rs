//! Android TUN backend driven by an `VpnService.establish()` fd.
//!
//! Android forbids direct `/dev/net/tun` open from app code. Apps must
//! instead build a `VpnService.Builder`, configure it, call
//! `establish()`, and pass the resulting `ParcelFileDescriptor`'s
//! integer fd through JNI to native code. Phase B's Tauri Android
//! plugin (`VpnServicePlugin.kt`) does exactly that, then sets
//! `PIM_TUN_FD=<int>` in the process environment before the daemon
//! constructs the [`PlatformTunInterface`].
//!
//! Address / netmask / mtu / route configuration happens entirely on
//! the Java side via `VpnService.Builder` calls (`addAddress`,
//! `addRoute`, `setMtu`, `addDnsServer`). The native side cannot
//! reach those knobs through the bare TUN fd, so `set_ip` /
//! `set_ipv6` / `set_mtu` / `up` / `add_default_route` are all
//! intentional no-ops here. Routing changes from the daemon (e.g.
//! split-default activation via the route-installer) likewise
//! require new `VpnService.Builder` calls and a re-`establish()` —
//! Phase B Step 4 wires that in `VpnServicePlugin.kt::reconfigure`.

use crate::interface::TunError;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

use tokio::io::unix::AsyncFd;
use tokio::io::Interest;
use tracing::{debug, info, warn};

/// Env var the JNI bridge sets to publish the `VpnService.establish()`
/// fd. Numeric ASCII; `<= 0` means absent.
const TUN_FD_ENV: &str = "PIM_TUN_FD";

/// Read `PIM_TUN_FD` and return the parsed fd. Returns `None` if the
/// var is absent or unparseable so the caller can downgrade to
/// `TunError::Unavailable` (matching Phase A behaviour).
fn tun_fd_from_env() -> Option<RawFd> {
    let raw = std::env::var_os(TUN_FD_ENV)?;
    let s = raw.to_str()?;
    let n: i32 = s.parse().ok()?;
    if n <= 0 {
        return None;
    }
    Some(n)
}

pub struct PlatformTunInterface {
    name: String,
    fd: AsyncFd<OwnedFd>,
}

impl PlatformTunInterface {
    /// On android the OS-assigned name is opaque; we stash whatever
    /// caller passed in for log readability. Reads the fd from
    /// `PIM_TUN_FD`. Returns `TunError::Unavailable` when the env var
    /// is missing or malformed (allows the daemon to start with TUN
    /// disabled if VPN consent was denied).
    pub fn create(name: &str) -> Result<Self, TunError> {
        let raw_fd = match tun_fd_from_env() {
            Some(fd) => fd,
            None => {
                warn!(
                    "android TUN: PIM_TUN_FD env var missing or invalid — \
                     VpnService.establish() must run before daemon start"
                );
                return Err(TunError::Unavailable);
            }
        };

        // Set non-blocking; Java side returns blocking fds by default.
        unsafe {
            let flags = libc::fcntl(raw_fd, libc::F_GETFL);
            if flags < 0 {
                return Err(TunError::Io(io::Error::last_os_error()));
            }
            if libc::fcntl(raw_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(TunError::Io(io::Error::last_os_error()));
            }
        }

        // Take ownership of the fd. The drop impl on `OwnedFd` closes
        // it when the daemon stops, which signals the Java side via
        // `ParcelFileDescriptor.detachFd()` semantics.
        let owned = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let fd = AsyncFd::with_interest(owned, Interest::READABLE | Interest::WRITABLE)
            .map_err(TunError::Io)?;
        info!(fd = raw_fd, "android TUN: adopted fd from VpnService");

        Ok(Self {
            name: name.to_string(),
            fd,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// No-op on android: addressing happens via `VpnService.Builder`
    /// before `establish()`. Logged at debug for diagnosis only.
    pub fn set_ip(&self, addr: Ipv4Addr, prefix_len: u8) -> Result<(), TunError> {
        debug!(
            ?addr,
            prefix_len, "android TUN: set_ip is a no-op (set via VpnService.Builder)"
        );
        Ok(())
    }

    pub fn set_ipv6(&self, addr: Ipv6Addr, prefix_len: u8) -> Result<(), TunError> {
        debug!(?addr, prefix_len, "android TUN: set_ipv6 is a no-op");
        Ok(())
    }

    pub fn set_mtu(&self, mtu: u32) -> Result<(), TunError> {
        debug!(
            mtu,
            "android TUN: set_mtu is a no-op (set via VpnService.Builder)"
        );
        Ok(())
    }

    pub fn up(&self) -> Result<(), TunError> {
        // VpnService.establish() returns an interface that's already
        // up; nothing to flip on the native side.
        Ok(())
    }

    pub fn down(&self) -> Result<(), TunError> {
        // The Java side tears down the VPN by closing its
        // ParcelFileDescriptor. We just close our side too — `Drop`
        // on `OwnedFd` does that.
        Ok(())
    }

    pub async fn read_packet(&self, buf: &mut [u8]) -> Result<usize, TunError> {
        use std::os::fd::AsRawFd;
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::read(
                        inner.get_ref().as_raw_fd(),
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                    )
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(result) => return result.map_err(TunError::Io),
                Err(_would_block) => continue,
            }
        }
    }

    pub async fn write_packet(&self, packet: &[u8]) -> Result<(), TunError> {
        use std::os::fd::AsRawFd;
        loop {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::write(
                        inner.get_ref().as_raw_fd(),
                        packet.as_ptr() as *const libc::c_void,
                        packet.len(),
                    )
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else if n as usize != packet.len() {
                    Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        format!("short write: {n} of {} bytes", packet.len()),
                    ))
                } else {
                    Ok(())
                }
            }) {
                Ok(result) => return result.map_err(TunError::Io),
                Err(_would_block) => continue,
            }
        }
    }

    /// No-op: routes are part of the `VpnService.Builder` config and
    /// changing them requires re-`establish()`. The route installer
    /// on android logs and skips.
    pub fn add_default_route(&self, _gateway_ip: Ipv4Addr) -> Result<(), TunError> {
        debug!("android TUN: add_default_route is a no-op (configure via VpnService.Builder)");
        Ok(())
    }

    pub fn add_default_ipv6_route(&self, _gateway_ip: Ipv6Addr) -> Result<(), TunError> {
        debug!("android TUN: add_default_ipv6_route is a no-op");
        Ok(())
    }

    pub fn remove_default_route(&self, _gateway_ip: Ipv4Addr) -> Result<(), TunError> {
        Ok(())
    }

    pub fn remove_default_ipv6_route(&self, _gateway_ip: Ipv6Addr) -> Result<(), TunError> {
        Ok(())
    }
}
