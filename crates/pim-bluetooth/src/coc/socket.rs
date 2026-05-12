//! Raw Linux Bluetooth socket helpers — `AF_BLUETOOTH` + `BTPROTO_L2CAP`.
//!
//! BLE-routed L2CAP CoC counterpart to `rfcomm/socket.rs`. The kernel
//! decides BR/EDR vs LE based on `sockaddr_l2.l2_bdaddr_type` — set
//! it to one of `BDADDR_LE_PUBLIC` / `BDADDR_LE_RANDOM` and the LE
//! controller carries the channel; leave it at `BDADDR_BREDR` and it
//! falls back to Classic L2CAP (not what we want, hence we always set
//! an LE type).
//!
//! Constants verified against `/usr/include/bluetooth/bluetooth.h` and
//! `/usr/include/bluetooth/l2cap.h` (BlueZ headers).
//!
//! ## SDU semantics
//!
//! `SOCK_STREAM` over L2CAP CoC concatenates SDU boundaries. The PIM
//! length-prefix frame codec ([`crate::frame`]) already handles framing,
//! so SEQPACKET buys nothing and constrains max payload to the
//! negotiated MTU — STREAM is what we want.

#![cfg(target_os = "linux")]

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use tokio::io::unix::AsyncFd;
use tokio::io::Interest;

use super::BdAddr;

/// `AF_BLUETOOTH` — protocol family value from `bits/socket.h`.
pub const AF_BLUETOOTH: libc::c_int = 31;
/// `BTPROTO_L2CAP` — L2CAP protocol value from `bluetooth.h`. libc
/// does not expose this on any target; defined locally.
pub const BTPROTO_L2CAP: libc::c_int = 0;

/// Linux kernel's `sockaddr_l2` layout (from
/// `/usr/include/bluetooth/l2cap.h`):
///
/// ```c
/// struct sockaddr_l2 {
///     sa_family_t     l2_family;
///     unsigned short  l2_psm;
///     bdaddr_t        l2_bdaddr;
///     unsigned short  l2_cid;
///     uint8_t         l2_bdaddr_type;
/// };
/// ```
///
/// `l2_psm` and `l2_cid` are little-endian-on-wire (stored via
/// `htobs`); on x86 (LE host) the native u16 representation is
/// identical, so we don't byte-swap. `bdaddr_t` keeps the same kernel
/// little-endian layout as `sockaddr_rc.rc_bdaddr` ("AA:BB:CC:DD:EE:FF"
/// → `[FF, EE, DD, CC, BB, AA]`).
///
/// Total size: 2 + 2 + 6 + 2 + 1 = 13 B; the struct picks up one
/// trailing pad to align to `sa_family_t`'s alignment, giving
/// `sizeof = 14`. The kernel enforces `addr_len == sizeof(sockaddr_l2)`
/// strictly on `connect(2)`, same as RFCOMM — `repr(C)` (not
/// `packed`) keeps the natural alignment so `mem::size_of::<SockaddrL2>()`
/// matches the C header.
#[repr(C)]
struct SockaddrL2 {
    l2_family: libc::sa_family_t,
    l2_psm: u16,
    l2_bdaddr: [u8; 6],
    l2_cid: u16,
    l2_bdaddr_type: u8,
}

/// Async wrapper around an L2CAP CoC listening socket.
pub struct CocListener {
    fd: AsyncFd<OwnedFd>,
}

/// Async wrapper around a connected L2CAP CoC stream socket.
pub struct CocStream {
    fd: AsyncFd<OwnedFd>,
}

impl CocListener {
    /// Bind a listening L2CAP CoC socket on `psm` (LE dynamic range
    /// `0x0080..=0x00FF`) on the system's default Bluetooth controller
    /// (`BDADDR_ANY`). Address type is set to `BDADDR_LE_PUBLIC` so
    /// the kernel binds on the LE controller; inbound connections are
    /// still accepted from peers using either public or random
    /// addresses (`bdaddr_type` on the listening socket only constrains
    /// the local controller selection).
    pub fn bind(psm: u16) -> io::Result<Self> {
        let fd = create_l2cap_socket()?;
        let mut addr: SockaddrL2 = unsafe { mem::zeroed() };
        addr.l2_family = AF_BLUETOOTH as libc::sa_family_t;
        addr.l2_psm = psm;
        addr.l2_bdaddr = [0u8; 6];
        addr.l2_cid = 0;
        addr.l2_bdaddr_type = super::BDADDR_LE_PUBLIC;
        // SAFETY: addr layout matches the kernel's `sockaddr_l2`.
        let rc = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                &addr as *const SockaddrL2 as *const libc::sockaddr,
                mem::size_of::<SockaddrL2>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        let rc = unsafe { libc::listen(fd.as_raw_fd(), 8) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        let async_fd = AsyncFd::new(fd)?;
        Ok(Self { fd: async_fd })
    }

    /// Accept the next inbound connection.
    pub async fn accept(&self) -> io::Result<(CocStream, BdAddr)> {
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|inner| do_accept(inner.as_raw_fd())) {
                Ok(Ok((fd, addr))) => {
                    return Ok((
                        CocStream {
                            fd: AsyncFd::new(fd)?,
                        },
                        addr,
                    ))
                }
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }
}

fn do_accept(listen_fd: RawFd) -> io::Result<(OwnedFd, BdAddr)> {
    let mut addr: SockaddrL2 = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<SockaddrL2>() as libc::socklen_t;
    let rc = unsafe {
        libc::accept4(
            listen_fd,
            &mut addr as *mut SockaddrL2 as *mut libc::sockaddr,
            &mut len,
            libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: rc is a valid fd we own.
    let owned = unsafe { OwnedFd::from_raw_fd(rc) };
    Ok((owned, addr.l2_bdaddr))
}

/// Connect to `bd_addr` on `psm`. `bdaddr_type` must be one of
/// `BDADDR_LE_PUBLIC` / `BDADDR_LE_RANDOM`; passing `BDADDR_BREDR`
/// falls back to Classic L2CAP and bypasses the LE controller.
pub async fn connect(bd_addr: BdAddr, psm: u16, bdaddr_type: u8) -> io::Result<CocStream> {
    let fd = create_l2cap_socket()?;
    let mut addr: SockaddrL2 = unsafe { mem::zeroed() };
    addr.l2_family = AF_BLUETOOTH as libc::sa_family_t;
    addr.l2_psm = psm;
    addr.l2_bdaddr = bd_addr;
    addr.l2_cid = 0;
    addr.l2_bdaddr_type = bdaddr_type;
    let rc = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            &addr as *const SockaddrL2 as *const libc::sockaddr,
            mem::size_of::<SockaddrL2>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(err);
        }
        // Wait for connect to finish (writable signal), then check
        // SO_ERROR. Mirrors `rfcomm::socket::connect` — including the
        // post-connect re-registration so the resulting AsyncFd is
        // armed for BOTH readability and writability. Without that,
        // the first read on the dialer side blocked forever and the
        // bridge deadlocked (same failure mode we hit on RFCOMM
        // pre-fix, captured in `rfcomm/socket.rs:170-184`).
        let waiter = AsyncFd::with_interest(fd, Interest::WRITABLE)?;
        let _ = waiter.writable().await?;
        let mut so_error: libc::c_int = 0;
        let mut so_len = mem::size_of::<libc::c_int>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                waiter.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                &mut so_error as *mut libc::c_int as *mut libc::c_void,
                &mut so_len,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        if so_error != 0 {
            return Err(io::Error::from_raw_os_error(so_error));
        }
        let inner = waiter.into_inner();
        return Ok(CocStream {
            fd: AsyncFd::new(inner)?,
        });
    }
    Ok(CocStream {
        fd: AsyncFd::new(fd)?,
    })
}

fn create_l2cap_socket() -> io::Result<OwnedFd> {
    // SOCK_STREAM (not SOCK_SEQPACKET) — the frame codec handles
    // boundaries; STREAM avoids the SEQPACKET MTU cap.
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            BTPROTO_L2CAP,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

impl CocStream {
    /// Read up to `buf.len()` bytes; awaits readability.
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::read(
                        inner.as_raw_fd(),
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
                Ok(r) => return r,
                Err(_would_block) => continue,
            }
        }
    }

    /// Write `buf` fully (loops on partial writes); awaits writability.
    pub async fn write_all(&self, mut buf: &[u8]) -> io::Result<()> {
        while !buf.is_empty() {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::write(
                        inner.as_raw_fd(),
                        buf.as_ptr() as *const libc::c_void,
                        buf.len(),
                    )
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(Ok(n)) => buf = &buf[n..],
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod sockaddr_layout_tests {
    use super::*;

    #[test]
    fn sockaddr_l2_size_matches_bluez_header() {
        // BlueZ `struct sockaddr_l2` is 14 B with natural alignment
        // (2 + 2 + 6 + 2 + 1 + 1 trailing pad). The kernel enforces
        // `addr_len == sizeof(sockaddr_l2)` strictly on connect(2);
        // a mismatch returns EINVAL synchronously, exactly the same
        // failure mode we caught on `sockaddr_rc` pre-fix. Lock it.
        assert_eq!(mem::size_of::<SockaddrL2>(), 14);
    }
}
