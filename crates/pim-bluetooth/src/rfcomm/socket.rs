//! Raw Linux Bluetooth socket helpers — `AF_BLUETOOTH` + `BTPROTO_RFCOMM`.
//!
//! `libc` does not yet expose the Bluetooth-specific constants for the
//! Apple targets; we define them locally and gate the whole module on
//! `target_os = "linux"`. Constants verified against Linux kernel
//! `include/bluetooth/bluetooth.h` and `include/bluetooth/rfcomm.h`.

#![cfg(target_os = "linux")]

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use tokio::io::unix::AsyncFd;
use tokio::io::Interest;

use super::BdAddr;

/// `AF_BLUETOOTH` — protocol family value from `bits/socket.h`.
pub const AF_BLUETOOTH: libc::c_int = 31;
/// `BTPROTO_RFCOMM` — RFCOMM stream protocol value from `bluetooth.h`.
pub const BTPROTO_RFCOMM: libc::c_int = 3;

/// Linux kernel's `sockaddr_rc` layout — `sa_family_t` + 6-byte
/// `bdaddr_t` (little-endian) + 1-byte channel.
///
/// MUST be `repr(C)` (not `packed`). The kernel's userspace header
/// declares only `bdaddr_t` packed; the outer `sockaddr_rc` keeps
/// natural alignment, so its `sizeof` is 10 bytes (2 + 6 + 1 + 1
/// trailing pad to align to `sa_family_t`'s alignment of 2).
/// `rfcomm_sock_connect` enforces `addr_len < sizeof(sockaddr_rc)`
/// strictly — passing the packed 9-byte size returns EINVAL.
/// Verified against a header-free C reference: same MAC, same
/// channel, the C connect succeeds with `sizeof = 10` while the
/// packed-9 Rust call fails synchronously with `Invalid argument`.
#[repr(C)]
struct SockaddrRc {
    rc_family: libc::sa_family_t,
    rc_bdaddr: [u8; 6],
    rc_channel: u8,
}

/// Async wrapper around an RFCOMM listening socket. `accept` yields
/// connected `RfcommStream`s along with the peer BD_ADDR.
pub struct RfcommListener {
    fd: AsyncFd<OwnedFd>,
}

/// Async wrapper around a connected RFCOMM stream socket. Implements
/// `tokio::io::AsyncRead` + `AsyncWrite` over a non-blocking fd.
pub struct RfcommStream {
    fd: AsyncFd<OwnedFd>,
}

impl RfcommListener {
    /// Bind a listening RFCOMM socket on `channel` (1..=30) on the
    /// system's first/default Bluetooth controller (`BDADDR_ANY`).
    pub fn bind(channel: u8) -> io::Result<Self> {
        let fd = create_rfcomm_socket()?;
        // Bind to BDADDR_ANY (all zero) on `channel`.
        let mut addr: SockaddrRc = unsafe { mem::zeroed() };
        addr.rc_family = AF_BLUETOOTH as libc::sa_family_t;
        addr.rc_bdaddr = [0u8; 6];
        addr.rc_channel = channel;
        // SAFETY: addr is `repr(C, packed)` matching the kernel layout;
        // length and family are valid for the rfcomm protocol.
        let rc = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                &addr as *const SockaddrRc as *const libc::sockaddr,
                mem::size_of::<SockaddrRc>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: backlog 8 is conservative.
        let rc = unsafe { libc::listen(fd.as_raw_fd(), 8) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        let async_fd = AsyncFd::new(fd)?;
        Ok(Self { fd: async_fd })
    }

    /// Accept the next inbound connection. Awaits readability then
    /// performs `accept(2)` non-blocking. Returns `(stream, peer_bdaddr)`.
    pub async fn accept(&self) -> io::Result<(RfcommStream, BdAddr)> {
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|inner| do_accept(inner.as_raw_fd())) {
                Ok(Ok((fd, addr))) => {
                    return Ok((
                        RfcommStream {
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
    let mut addr: SockaddrRc = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<SockaddrRc>() as libc::socklen_t;
    // Use accept4 with SOCK_NONBLOCK + SOCK_CLOEXEC for atomic flags.
    let rc = unsafe {
        libc::accept4(
            listen_fd,
            &mut addr as *mut SockaddrRc as *mut libc::sockaddr,
            &mut len,
            libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: rc is a valid fd we own.
    let owned = unsafe { OwnedFd::from_raw_fd(rc) };
    Ok((owned, addr.rc_bdaddr))
}

/// Connect to `bd_addr` on `channel`. Async; uses non-blocking connect.
pub async fn connect(bd_addr: BdAddr, channel: u8) -> io::Result<RfcommStream> {
    let fd = create_rfcomm_socket()?;
    let mut addr: SockaddrRc = unsafe { mem::zeroed() };
    addr.rc_family = AF_BLUETOOTH as libc::sa_family_t;
    addr.rc_bdaddr = bd_addr;
    addr.rc_channel = channel;
    // SAFETY: addr layout matches kernel.
    let rc = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            &addr as *const SockaddrRc as *const libc::sockaddr,
            mem::size_of::<SockaddrRc>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        let err = io::Error::last_os_error();
        // EINPROGRESS means non-blocking connect started; await
        // writability + check SO_ERROR.
        if err.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(err);
        }
        let async_fd = AsyncFd::with_interest(fd, Interest::WRITABLE)?;
        let _ = async_fd.writable().await?;
        // Check SO_ERROR.
        let mut so_error: libc::c_int = 0;
        let mut so_len = mem::size_of::<libc::c_int>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                async_fd.as_raw_fd(),
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
        return Ok(RfcommStream { fd: async_fd });
    }
    Ok(RfcommStream {
        fd: AsyncFd::new(fd)?,
    })
}

fn create_rfcomm_socket() -> io::Result<OwnedFd> {
    // SAFETY: socket(2) with the BT family + RFCOMM proto is safe to
    // call; non-blocking + cloexec set atomically via type flags.
    let fd = unsafe {
        libc::socket(
            AF_BLUETOOTH,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            BTPROTO_RFCOMM,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a valid kernel descriptor we own.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

impl RfcommStream {
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
