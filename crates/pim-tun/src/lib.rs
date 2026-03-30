//! Linux TUN interface management used by the daemon dataplane.

#![warn(missing_docs)]

use std::fs::{File, OpenOptions};
use std::io;
use std::net::Ipv4Addr;
use std::os::unix::io::AsRawFd;

use tokio::io::unix::AsyncFd;
use tracing::{debug, info};

/// Errors from TUN operations.
#[derive(Debug, thiserror::Error)]
pub enum TunError {
    /// Opening or operating on the device failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// An ioctl returned an error from the kernel.
    #[error("ioctl {0} failed: {1}")]
    Ioctl(String, io::Error),
    /// The requested interface name exceeded Linux `IFNAMSIZ - 1`.
    #[error("interface name too long (max 15 chars)")]
    NameTooLong,
    /// `/dev/net/tun` is unavailable on this host.
    #[error("TUN device not available: /dev/net/tun missing (requires Linux + CAP_NET_ADMIN)")]
    Unavailable,
}

// ── TUN/TAP ioctl ─────────────────────────────────────────────────────────────
// TUNSETIFF = _IOW('T', 202, int)  →  0x400454ca
const TUNSETIFF: libc::c_ulong = 0x4004_54ca;
const IFF_TUN: i16 = 0x0001;
const IFF_NO_PI: i16 = 0x1000; // no packet-info prefix

// ── Socket interface ioctls ───────────────────────────────────────────────────
const SIOCSIFADDR: libc::c_ulong = 0x8916;
const SIOCSIFNETMASK: libc::c_ulong = 0x891c;
const SIOCSIFMTU: libc::c_ulong = 0x8922;
const SIOCGIFFLAGS: libc::c_ulong = 0x8913;
const SIOCSIFFLAGS: libc::c_ulong = 0x8914;
const IFF_UP: i16 = 0x0001;
const IFF_RUNNING: i16 = 0x0040;

const IFNAMSIZ: usize = 16;

/// C-compatible ifreq structure (name + 16-byte union).
///
/// Layout: ifr_name[16] + ifr_ifru[16].
/// The union field is interpreted depending on the ioctl being called.
#[repr(C)]
struct Ifreq {
    ifr_name: [u8; IFNAMSIZ],
    ifr_union: [u8; 16],
}

impl Ifreq {
    fn new(name: &str) -> Result<Self, TunError> {
        let bytes = name.as_bytes();
        if bytes.len() >= IFNAMSIZ {
            return Err(TunError::NameTooLong);
        }
        let mut ifr = Self {
            ifr_name: [0; IFNAMSIZ],
            ifr_union: [0; 16],
        };
        ifr.ifr_name[..bytes.len()].copy_from_slice(bytes);
        Ok(ifr)
    }

    /// Read interface name back from the kernel (written after TUNSETIFF).
    fn name_str(&self) -> String {
        let end = self
            .ifr_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(IFNAMSIZ);
        String::from_utf8_lossy(&self.ifr_name[..end]).to_string()
    }

    /// Set ifr_flags (IFF_TUN | IFF_NO_PI) for TUNSETIFF.
    fn set_tun_flags(&mut self) {
        let flags: i16 = IFF_TUN | IFF_NO_PI;
        self.ifr_union[..2].copy_from_slice(&flags.to_ne_bytes());
    }

    /// Set ifr_ifru to a sockaddr_in for SIOCSIFADDR / SIOCSIFNETMASK.
    fn set_sockaddr_in(&mut self, addr: Ipv4Addr) {
        self.ifr_union.fill(0);
        // sin_family (AF_INET = 2) at offset 0 as u16 native-endian
        let family = (libc::AF_INET as u16).to_ne_bytes();
        self.ifr_union[0..2].copy_from_slice(&family);
        // sin_port = 0 (already zeroed)
        // sin_addr at offset 4
        self.ifr_union[4..8].copy_from_slice(&addr.octets());
    }

    /// Set ifr_flags for SIOCSIFFLAGS.
    fn set_flags(&mut self, flags: i16) {
        self.ifr_union.fill(0);
        self.ifr_union[..2].copy_from_slice(&flags.to_ne_bytes());
    }

    /// Read ifr_flags after SIOCGIFFLAGS.
    fn get_flags(&self) -> i16 {
        i16::from_ne_bytes([self.ifr_union[0], self.ifr_union[1]])
    }

    /// Set ifr_mtu for SIOCSIFMTU.
    fn set_mtu(&mut self, mtu: i32) {
        self.ifr_union.fill(0);
        self.ifr_union[..4].copy_from_slice(&mtu.to_ne_bytes());
    }
}

/// Execute an ioctl on `fd` with `request` and an Ifreq argument.
fn do_ioctl(fd: libc::c_int, request: libc::c_ulong, ifr: &mut Ifreq) -> Result<(), TunError> {
    let ret = unsafe { libc::ioctl(fd, request, ifr as *mut Ifreq) };
    if ret < 0 {
        Err(TunError::Ioctl(
            format!("0x{request:08x}"),
            io::Error::last_os_error(),
        ))
    } else {
        Ok(())
    }
}

/// Open a temporary AF_INET SOCK_DGRAM socket for interface configuration ioctls.
struct ConfigSocket(libc::c_int);

impl ConfigSocket {
    fn open() -> Result<Self, TunError> {
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            Err(TunError::Io(io::Error::last_os_error()))
        } else {
            Ok(Self(fd))
        }
    }

    fn fd(&self) -> libc::c_int {
        self.0
    }
}

impl Drop for ConfigSocket {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

/// An asynchronous Linux TUN network interface.
///
/// Requires `CAP_NET_ADMIN` (or running as root) to create.
/// The interface is automatically destroyed when this struct is dropped (the
/// kernel removes a TUN/TAP interface when all file descriptors to it are closed).
pub struct TunInterface {
    name: String,
    fd: AsyncFd<File>,
}

impl TunInterface {
    /// Create a TUN device with the requested name.
    ///
    /// The actual name assigned by the kernel is available via [`name()`].
    /// Call [`set_ip`], [`set_mtu`], and [`up`] before using the interface.
    pub fn create(name: &str) -> Result<Self, TunError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/net/tun")
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    TunError::Unavailable
                } else {
                    TunError::Io(e)
                }
            })?;

        let raw_fd = file.as_raw_fd();

        // Configure as TUN (no packet-info prefix)
        let mut ifr = Ifreq::new(name)?;
        ifr.set_tun_flags();
        do_ioctl(raw_fd, TUNSETIFF, &mut ifr)?;

        let assigned_name = ifr.name_str();
        info!(name = %assigned_name, "TUN interface created");

        // Enable non-blocking so tokio can poll it
        unsafe {
            let flags = libc::fcntl(raw_fd, libc::F_GETFL);
            if flags < 0 {
                return Err(TunError::Io(io::Error::last_os_error()));
            }
            if libc::fcntl(raw_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(TunError::Io(io::Error::last_os_error()));
            }
        }

        Ok(Self {
            name: assigned_name,
            fd: AsyncFd::new(file)?,
        })
    }

    /// The kernel-assigned interface name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Assign an IPv4 address and prefix length (CIDR notation, e.g. 24 for /24).
    pub fn set_ip(&self, addr: Ipv4Addr, prefix_len: u8) -> Result<(), TunError> {
        let sock = ConfigSocket::open()?;

        let mut ifr = Ifreq::new(&self.name)?;
        ifr.set_sockaddr_in(addr);
        do_ioctl(sock.fd(), SIOCSIFADDR, &mut ifr)?;

        let mask = prefix_to_mask(prefix_len);
        let mut ifr = Ifreq::new(&self.name)?;
        ifr.set_sockaddr_in(mask);
        do_ioctl(sock.fd(), SIOCSIFNETMASK, &mut ifr)?;

        debug!(addr = %addr, prefix = prefix_len, iface = %self.name, "IP address set");
        Ok(())
    }

    /// Set the interface MTU (bytes).
    pub fn set_mtu(&self, mtu: u32) -> Result<(), TunError> {
        let sock = ConfigSocket::open()?;

        let mut ifr = Ifreq::new(&self.name)?;
        ifr.set_mtu(mtu as i32);
        do_ioctl(sock.fd(), SIOCSIFMTU, &mut ifr)?;

        debug!(mtu = mtu, iface = %self.name, "MTU set");
        Ok(())
    }

    /// Bring the interface up (IFF_UP | IFF_RUNNING).
    pub fn up(&self) -> Result<(), TunError> {
        self.change_flags(IFF_UP | IFF_RUNNING, 0)
    }

    /// Bring the interface down.
    pub fn down(&self) -> Result<(), TunError> {
        self.change_flags(0, IFF_UP | IFF_RUNNING)
    }

    fn change_flags(&self, set: i16, clear: i16) -> Result<(), TunError> {
        let sock = ConfigSocket::open()?;

        // Read current flags
        let mut ifr = Ifreq::new(&self.name)?;
        do_ioctl(sock.fd(), SIOCGIFFLAGS, &mut ifr)?;
        let current = ifr.get_flags();

        let new_flags = (current & !clear) | set;
        ifr.set_flags(new_flags);
        do_ioctl(sock.fd(), SIOCSIFFLAGS, &mut ifr)?;

        debug!(flags = new_flags, iface = %self.name, "interface flags updated");
        Ok(())
    }

    /// Asynchronously read one IP packet from the TUN device.
    ///
    /// `buf` must be large enough for the largest expected packet (e.g. 65536 bytes).
    /// Returns the number of bytes written into `buf`.
    pub async fn read_packet(&self, buf: &mut [u8]) -> Result<usize, TunError> {
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
                Ok(result) => return result.map_err(TunError::Io),
                Err(_would_block) => continue,
            }
        }
    }

    /// Asynchronously write one IP packet to the TUN device.
    pub async fn write_packet(&self, packet: &[u8]) -> Result<(), TunError> {
        loop {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::write(
                        inner.as_raw_fd(),
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

    /// Add a default route via `gateway_ip` on this interface.
    ///
    /// Shells out to `ip route add <cidr> via <gw> dev <iface> onlink`.
    /// Uses 0.0.0.0/1 and 128.0.0.0/1 to override the default route without replacing it.
    pub fn add_default_route(&self, gateway_ip: Ipv4Addr) -> Result<(), TunError> {
        let gw_str = gateway_ip.to_string();
        for cidr in ["0.0.0.0/1", "128.0.0.0/1"] {
            let status = std::process::Command::new("ip")
                .args(default_route_args(cidr, &gw_str, &self.name))
                .status()?;

            if !status.success() {
                return Err(TunError::Ioctl(
                    "ip route add".into(),
                    io::Error::new(
                        io::ErrorKind::Other,
                        format!("ip route add {} failed", cidr),
                    ),
                ));
            }
        }

        debug!(gateway = %gateway_ip, iface = %self.name, "default routes (0.0.0.0/1, 128.0.0.0/1) added");
        Ok(())
    }
}

fn default_route_args<'a>(cidr: &'a str, gateway: &'a str, iface: &'a str) -> [&'a str; 8] {
    ["route", "add", cidr, "via", gateway, "dev", iface, "onlink"]
}

/// Convert a CIDR prefix length to an IPv4 netmask address.
fn prefix_to_mask(prefix_len: u8) -> Ipv4Addr {
    if prefix_len == 0 {
        return Ipv4Addr::new(0, 0, 0, 0);
    }
    if prefix_len >= 32 {
        return Ipv4Addr::new(255, 255, 255, 255);
    }
    let mask: u32 = !((1u32 << (32 - prefix_len)) - 1);
    Ipv4Addr::from(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_to_mask_standard() {
        assert_eq!(prefix_to_mask(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(prefix_to_mask(16), Ipv4Addr::new(255, 255, 0, 0));
        assert_eq!(prefix_to_mask(8), Ipv4Addr::new(255, 0, 0, 0));
        assert_eq!(prefix_to_mask(30), Ipv4Addr::new(255, 255, 255, 252));
        assert_eq!(prefix_to_mask(0), Ipv4Addr::new(0, 0, 0, 0));
        assert_eq!(prefix_to_mask(32), Ipv4Addr::new(255, 255, 255, 255));
    }

    #[test]
    fn ifreq_name_round_trip() {
        let ifr = Ifreq::new("pim0").unwrap();
        assert_eq!(ifr.name_str(), "pim0");
    }

    #[test]
    fn ifreq_name_too_long_rejected() {
        // 15 chars = OK (16 with null terminator = exactly IFNAMSIZ)
        assert!(Ifreq::new("123456789012345").is_ok());
        // 16 chars = too long
        assert!(matches!(
            Ifreq::new("1234567890123456"),
            Err(TunError::NameTooLong)
        ));
    }

    #[test]
    fn ifreq_flags_set_get() {
        let mut ifr = Ifreq::new("test").unwrap();
        ifr.set_flags(IFF_UP | IFF_RUNNING);
        assert_eq!(ifr.get_flags(), IFF_UP | IFF_RUNNING);
    }

    #[test]
    fn ifreq_sockaddr_in_addr_bytes() {
        let mut ifr = Ifreq::new("test").unwrap();
        let addr = Ipv4Addr::new(10, 77, 0, 5);
        ifr.set_sockaddr_in(addr);
        // Verify the address bytes are at offset 4
        assert_eq!(&ifr.ifr_union[4..8], &[10, 77, 0, 5]);
        // Verify AF_INET (2) is set at offset 0
        let family = u16::from_ne_bytes([ifr.ifr_union[0], ifr.ifr_union[1]]);
        assert_eq!(family, libc::AF_INET as u16);
    }

    #[test]
    fn default_route_args_include_onlink_for_lower_half() {
        assert_eq!(
            default_route_args("0.0.0.0/1", "10.77.0.1", "pim0"),
            [
                "route",
                "add",
                "0.0.0.0/1",
                "via",
                "10.77.0.1",
                "dev",
                "pim0",
                "onlink"
            ]
        );
    }

    #[test]
    fn default_route_args_include_onlink_for_upper_half() {
        assert_eq!(
            default_route_args("128.0.0.0/1", "10.77.0.1", "pim0"),
            [
                "route",
                "add",
                "128.0.0.0/1",
                "via",
                "10.77.0.1",
                "dev",
                "pim0",
                "onlink"
            ]
        );
    }

    // TUN creation tests require CAP_NET_ADMIN and are run in Docker.
    // See testing-guide.md for multi-node test setup.
    #[test]
    #[ignore = "requires CAP_NET_ADMIN"]
    fn create_tun_interface() {
        let tun = TunInterface::create("pim-test0").unwrap();
        assert_eq!(tun.name(), "pim-test0");
    }
}
