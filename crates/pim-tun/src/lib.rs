//! Cross-platform TUN interface management used by the daemon dataplane.

#![warn(missing_docs)]

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};

/// Errors from TUN operations.
#[derive(Debug, thiserror::Error)]
pub enum TunError {
    /// Opening or operating on the device failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// An ioctl returned an error from the kernel.
    #[error("ioctl {0} failed: {1}")]
    Ioctl(String, io::Error),
    /// The requested interface name exceeded the platform limit.
    #[error("interface name too long (max 15 chars)")]
    NameTooLong,
    /// The host does not expose a usable TUN facility.
    #[error("TUN device not available on this host")]
    Unavailable,
    /// The requested interface name is not usable on this platform.
    #[error("unsupported interface name for this platform: {0}")]
    UnsupportedInterfaceName(String),
}

/// An asynchronous TUN network interface.
pub struct TunInterface {
    inner: platform::PlatformTunInterface,
}

impl TunInterface {
    /// Create a TUN device with the requested name.
    pub fn create(name: &str) -> Result<Self, TunError> {
        Ok(Self {
            inner: platform::PlatformTunInterface::create(name)?,
        })
    }

    /// The kernel-assigned interface name.
    pub fn name(&self) -> &str {
        self.inner.name()
    }

    /// Assign an IPv4 address and prefix length.
    pub fn set_ip(&self, addr: Ipv4Addr, prefix_len: u8) -> Result<(), TunError> {
        self.inner.set_ip(addr, prefix_len)
    }

    /// Assign an IPv6 address and prefix length.
    pub fn set_ipv6(&self, addr: Ipv6Addr, prefix_len: u8) -> Result<(), TunError> {
        self.inner.set_ipv6(addr, prefix_len)
    }

    /// Set the interface MTU in bytes.
    pub fn set_mtu(&self, mtu: u32) -> Result<(), TunError> {
        self.inner.set_mtu(mtu)
    }

    /// Bring the interface up.
    pub fn up(&self) -> Result<(), TunError> {
        self.inner.up()
    }

    /// Bring the interface down.
    pub fn down(&self) -> Result<(), TunError> {
        self.inner.down()
    }

    /// Asynchronously read one IP packet from the TUN device.
    pub async fn read_packet(&self, buf: &mut [u8]) -> Result<usize, TunError> {
        self.inner.read_packet(buf).await
    }

    /// Asynchronously write one IP packet to the TUN device.
    pub async fn write_packet(&self, packet: &[u8]) -> Result<(), TunError> {
        self.inner.write_packet(packet).await
    }

    /// Add split-default routes on this interface.
    pub fn add_default_route(&self, gateway_ip: Ipv4Addr) -> Result<(), TunError> {
        self.inner.add_default_route(gateway_ip)
    }

    /// Add split-default IPv6 routes on this interface.
    pub fn add_default_ipv6_route(&self, gateway_ip: Ipv6Addr) -> Result<(), TunError> {
        self.inner.add_default_ipv6_route(gateway_ip)
    }

    /// Remove split-default routes on this interface.
    pub fn remove_default_route(&self, gateway_ip: Ipv4Addr) -> Result<(), TunError> {
        self.inner.remove_default_route(gateway_ip)
    }

    /// Remove split-default IPv6 routes on this interface.
    pub fn remove_default_ipv6_route(&self, gateway_ip: Ipv6Addr) -> Result<(), TunError> {
        self.inner.remove_default_ipv6_route(gateway_ip)
    }
}

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

fn split_default_cidrs() -> [&'static str; 2] {
    ["0.0.0.0/1", "128.0.0.0/1"]
}

fn split_default_ipv6_cidrs() -> [&'static str; 2] {
    ["::/1", "8000::/1"]
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{prefix_to_mask, split_default_cidrs, split_default_ipv6_cidrs, TunError};
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::os::fd::AsRawFd;

    use tokio::io::unix::AsyncFd;
    use tracing::{debug, info};

    const TUNSETIFF: libc::Ioctl = 0x4004_54ca;
    const IFF_TUN: i16 = 0x0001;
    const IFF_NO_PI: i16 = 0x1000;

    const SIOCSIFADDR: libc::Ioctl = 0x8916;
    const SIOCSIFNETMASK: libc::Ioctl = 0x891c;
    const SIOCSIFMTU: libc::Ioctl = 0x8922;
    const SIOCGIFFLAGS: libc::Ioctl = 0x8913;
    const SIOCSIFFLAGS: libc::Ioctl = 0x8914;
    const IFF_UP: i16 = 0x0001;
    const IFF_RUNNING: i16 = 0x0040;

    const IFNAMSIZ: usize = 16;

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

        fn name_str(&self) -> String {
            let end = self
                .ifr_name
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(IFNAMSIZ);
            String::from_utf8_lossy(&self.ifr_name[..end]).to_string()
        }

        fn set_tun_flags(&mut self) {
            let flags: i16 = IFF_TUN | IFF_NO_PI;
            self.ifr_union[..2].copy_from_slice(&flags.to_ne_bytes());
        }

        fn set_sockaddr_in(&mut self, addr: Ipv4Addr) {
            self.ifr_union.fill(0);
            let family = (libc::AF_INET as u16).to_ne_bytes();
            self.ifr_union[0..2].copy_from_slice(&family);
            self.ifr_union[4..8].copy_from_slice(&addr.octets());
        }

        fn set_flags(&mut self, flags: i16) {
            self.ifr_union.fill(0);
            self.ifr_union[..2].copy_from_slice(&flags.to_ne_bytes());
        }

        fn get_flags(&self) -> i16 {
            i16::from_ne_bytes([self.ifr_union[0], self.ifr_union[1]])
        }

        fn set_mtu(&mut self, mtu: i32) {
            self.ifr_union.fill(0);
            self.ifr_union[..4].copy_from_slice(&mtu.to_ne_bytes());
        }
    }

    fn do_ioctl(fd: libc::c_int, request: libc::Ioctl, ifr: &mut Ifreq) -> Result<(), TunError> {
        let ret = unsafe { libc::ioctl(fd, request, ifr as *mut Ifreq) };
        if ret < 0 {
            Err(TunError::Ioctl(
                format!("0x{:08x}", request),
                io::Error::last_os_error(),
            ))
        } else {
            Ok(())
        }
    }

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

    pub struct PlatformTunInterface {
        name: String,
        fd: AsyncFd<File>,
    }

    impl PlatformTunInterface {
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
            let mut ifr = Ifreq::new(name)?;
            ifr.set_tun_flags();
            do_ioctl(raw_fd, TUNSETIFF, &mut ifr)?;

            let assigned_name = ifr.name_str();
            info!(name = %assigned_name, "TUN interface created");

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

        pub fn name(&self) -> &str {
            &self.name
        }

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

        pub fn set_ipv6(&self, addr: Ipv6Addr, prefix_len: u8) -> Result<(), TunError> {
            let addr_str = format!("{addr}/{prefix_len}");
            let status = std::process::Command::new("ip")
                .args(["-6", "addr", "replace", &addr_str, "dev", &self.name])
                .status()?;
            if !status.success() {
                return Err(TunError::Ioctl(
                    "ip -6 addr replace".into(),
                    io::Error::other(format!("ip -6 addr replace {addr_str} failed")),
                ));
            }

            debug!(addr = %addr, prefix = prefix_len, iface = %self.name, "IPv6 address set");
            Ok(())
        }

        pub fn set_mtu(&self, mtu: u32) -> Result<(), TunError> {
            let sock = ConfigSocket::open()?;

            let mut ifr = Ifreq::new(&self.name)?;
            ifr.set_mtu(mtu as i32);
            do_ioctl(sock.fd(), SIOCSIFMTU, &mut ifr)?;

            debug!(mtu, iface = %self.name, "MTU set");
            Ok(())
        }

        pub fn up(&self) -> Result<(), TunError> {
            self.change_flags(IFF_UP | IFF_RUNNING, 0)
        }

        pub fn down(&self) -> Result<(), TunError> {
            self.change_flags(0, IFF_UP | IFF_RUNNING)
        }

        fn change_flags(&self, set: i16, clear: i16) -> Result<(), TunError> {
            let sock = ConfigSocket::open()?;
            let mut ifr = Ifreq::new(&self.name)?;
            do_ioctl(sock.fd(), SIOCGIFFLAGS, &mut ifr)?;
            let current = ifr.get_flags();

            let new_flags = (current & !clear) | set;
            ifr.set_flags(new_flags);
            do_ioctl(sock.fd(), SIOCSIFFLAGS, &mut ifr)?;

            debug!(flags = new_flags, iface = %self.name, "interface flags updated");
            Ok(())
        }

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

        pub fn add_default_route(&self, gateway_ip: Ipv4Addr) -> Result<(), TunError> {
            let gw_str = gateway_ip.to_string();
            for cidr in split_default_cidrs() {
                let status = std::process::Command::new("ip")
                    .args([
                        "route", "add", cidr, "via", &gw_str, "dev", &self.name, "onlink",
                    ])
                    .status()?;

                if !status.success() {
                    return Err(TunError::Ioctl(
                        "ip route add".into(),
                        io::Error::other(format!("ip route add {cidr} failed")),
                    ));
                }
            }

            debug!(gateway = %gateway_ip, iface = %self.name, "split-default routes added");
            Ok(())
        }

        pub fn add_default_ipv6_route(&self, gateway_ip: Ipv6Addr) -> Result<(), TunError> {
            let gw_str = gateway_ip.to_string();
            for cidr in split_default_ipv6_cidrs() {
                let status = std::process::Command::new("ip")
                    .args([
                        "-6", "route", "replace", cidr, "via", &gw_str, "dev", &self.name, "onlink",
                    ])
                    .status()?;

                if !status.success() {
                    return Err(TunError::Ioctl(
                        "ip -6 route replace".into(),
                        io::Error::other(format!("ip -6 route replace {cidr} failed")),
                    ));
                }
            }

            debug!(gateway = %gateway_ip, iface = %self.name, "split-default IPv6 routes added");
            Ok(())
        }

        pub fn remove_default_route(&self, gateway_ip: Ipv4Addr) -> Result<(), TunError> {
            let gw_str = gateway_ip.to_string();
            for cidr in split_default_cidrs() {
                let _ = std::process::Command::new("ip")
                    .args(["route", "del", cidr, "via", &gw_str, "dev", &self.name])
                    .status()?;
            }

            debug!(gateway = %gateway_ip, iface = %self.name, "split-default routes removed");
            Ok(())
        }

        pub fn remove_default_ipv6_route(&self, gateway_ip: Ipv6Addr) -> Result<(), TunError> {
            let gw_str = gateway_ip.to_string();
            for cidr in split_default_ipv6_cidrs() {
                let _ = std::process::Command::new("ip")
                    .args([
                        "-6", "route", "del", cidr, "via", &gw_str, "dev", &self.name,
                    ])
                    .status()?;
            }

            debug!(gateway = %gateway_ip, iface = %self.name, "split-default IPv6 routes removed");
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn ifreq_name_round_trip() {
            let ifr = Ifreq::new("pim0").unwrap();
            assert_eq!(ifr.name_str(), "pim0");
        }

        #[test]
        fn ifreq_name_too_long_rejected() {
            assert!(Ifreq::new("123456789012345").is_ok());
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
            assert_eq!(&ifr.ifr_union[4..8], &[10, 77, 0, 5]);
            let family = u16::from_ne_bytes([ifr.ifr_union[0], ifr.ifr_union[1]]);
            assert_eq!(family, libc::AF_INET as u16);
        }

        #[test]
        #[ignore = "requires CAP_NET_ADMIN"]
        fn create_tun_interface() {
            let tun = PlatformTunInterface::create("pim-test0").unwrap();
            assert_eq!(tun.name(), "pim-test0");
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{prefix_to_mask, split_default_cidrs, split_default_ipv6_cidrs, TunError};
    use std::ffi::CStr;
    use std::io;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::process::Command;

    use tokio::io::unix::AsyncFd;
    use tracing::{debug, info};

    const CTLIOCGINFO: libc::c_ulong = 0xc064_4e03;
    const UTUN_OPT_IFNAME: libc::c_int = 2;
    const MAX_KCTL_NAME: usize = 96;

    #[repr(C)]
    struct CtlInfo {
        ctl_id: u32,
        ctl_name: [libc::c_char; MAX_KCTL_NAME],
    }

    #[repr(C)]
    struct SockaddrCtl {
        sc_len: u8,
        sc_family: u8,
        ss_sysaddr: u16,
        sc_id: u32,
        sc_unit: u32,
        sc_reserved: [u32; 5],
    }

    pub struct PlatformTunInterface {
        name: String,
        fd: AsyncFd<OwnedFd>,
    }

    impl PlatformTunInterface {
        pub fn create(name: &str) -> Result<Self, TunError> {
            let unit = parse_utun_unit(name)?;
            let fd =
                unsafe { libc::socket(libc::PF_SYSTEM, libc::SOCK_DGRAM, libc::SYSPROTO_CONTROL) };
            if fd < 0 {
                return Err(TunError::Io(io::Error::last_os_error()));
            }

            let result = (|| {
                let mut ctl_info = CtlInfo {
                    ctl_id: 0,
                    ctl_name: [0; MAX_KCTL_NAME],
                };
                let name_bytes = b"com.apple.net.utun_control\0";
                for (idx, byte) in name_bytes.iter().enumerate() {
                    ctl_info.ctl_name[idx] = *byte as libc::c_char;
                }

                let rc = unsafe { libc::ioctl(fd, CTLIOCGINFO, &mut ctl_info) };
                if rc < 0 {
                    return Err(TunError::Ioctl(
                        "CTLIOCGINFO".into(),
                        io::Error::last_os_error(),
                    ));
                }

                let addr = SockaddrCtl {
                    sc_len: std::mem::size_of::<SockaddrCtl>() as u8,
                    sc_family: libc::AF_SYSTEM as u8,
                    ss_sysaddr: libc::AF_SYS_CONTROL as u16,
                    sc_id: ctl_info.ctl_id,
                    sc_unit: unit,
                    sc_reserved: [0; 5],
                };

                let rc = unsafe {
                    libc::connect(
                        fd,
                        &addr as *const _ as *const libc::sockaddr,
                        std::mem::size_of::<SockaddrCtl>() as libc::socklen_t,
                    )
                };
                if rc < 0 {
                    return Err(TunError::Io(io::Error::last_os_error()));
                }

                unsafe {
                    let flags = libc::fcntl(fd, libc::F_GETFL);
                    if flags < 0 {
                        return Err(TunError::Io(io::Error::last_os_error()));
                    }
                    if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                        return Err(TunError::Io(io::Error::last_os_error()));
                    }
                }

                let mut name_buf = [0u8; libc::IFNAMSIZ];
                let mut name_len = name_buf.len() as libc::socklen_t;
                let rc = unsafe {
                    libc::getsockopt(
                        fd,
                        libc::SYSPROTO_CONTROL,
                        UTUN_OPT_IFNAME,
                        name_buf.as_mut_ptr() as *mut libc::c_void,
                        &mut name_len,
                    )
                };
                if rc < 0 {
                    return Err(TunError::Io(io::Error::last_os_error()));
                }

                let assigned_name = CStr::from_bytes_until_nul(&name_buf)
                    .map_err(|_| {
                        TunError::Io(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "utun name missing NUL terminator",
                        ))
                    })?
                    .to_string_lossy()
                    .into_owned();
                info!(requested = %name, assigned = %assigned_name, "TUN interface created");

                let fd = unsafe { OwnedFd::from_raw_fd(fd) };
                Ok(Self {
                    name: assigned_name,
                    fd: AsyncFd::new(fd)?,
                })
            })();

            if result.is_err() {
                unsafe { libc::close(fd) };
            }
            result
        }

        pub fn name(&self) -> &str {
            &self.name
        }

        pub fn set_ip(&self, addr: Ipv4Addr, prefix_len: u8) -> Result<(), TunError> {
            let addr_str = addr.to_string();
            let mask_str = prefix_to_mask(prefix_len).to_string();
            run_command(
                "ifconfig",
                &[
                    &self.name, "inet", &addr_str, &addr_str, "netmask", &mask_str,
                ],
                "ifconfig inet",
            )?;
            debug!(addr = %addr, prefix = prefix_len, iface = %self.name, "IP address set");
            Ok(())
        }

        pub fn set_ipv6(&self, addr: Ipv6Addr, prefix_len: u8) -> Result<(), TunError> {
            let addr_str = format!("{addr}/{prefix_len}");
            run_command(
                "ifconfig",
                &[&self.name, "inet6", &addr_str, "alias"],
                "ifconfig inet6",
            )?;
            debug!(addr = %addr, prefix = prefix_len, iface = %self.name, "IPv6 address set");
            Ok(())
        }

        pub fn set_mtu(&self, mtu: u32) -> Result<(), TunError> {
            let mtu_str = mtu.to_string();
            run_command("ifconfig", &[&self.name, "mtu", &mtu_str], "ifconfig mtu")?;
            debug!(mtu, iface = %self.name, "MTU set");
            Ok(())
        }

        pub fn up(&self) -> Result<(), TunError> {
            run_command("ifconfig", &[&self.name, "up"], "ifconfig up")
        }

        pub fn down(&self) -> Result<(), TunError> {
            run_command("ifconfig", &[&self.name, "down"], "ifconfig down")
        }

        pub async fn read_packet(&self, buf: &mut [u8]) -> Result<usize, TunError> {
            let mut frame = vec![0u8; buf.len() + 4];
            loop {
                let mut guard = self.fd.readable().await?;
                match guard.try_io(|inner| {
                    let n = unsafe {
                        libc::read(
                            inner.as_raw_fd(),
                            frame.as_mut_ptr() as *mut libc::c_void,
                            frame.len(),
                        )
                    };
                    if n < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(n as usize)
                    }
                }) {
                    Ok(result) => {
                        let n = result.map_err(TunError::Io)?;
                        if n < 4 {
                            return Err(TunError::Io(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "short utun frame",
                            )));
                        }
                        let packet_len = n - 4;
                        buf[..packet_len].copy_from_slice(&frame[4..n]);
                        return Ok(packet_len);
                    }
                    Err(_would_block) => continue,
                }
            }
        }

        pub async fn write_packet(&self, packet: &[u8]) -> Result<(), TunError> {
            let mut frame = Vec::with_capacity(packet.len() + 4);
            let family = match packet.first().map(|byte| byte >> 4) {
                Some(6) => libc::AF_INET6,
                _ => libc::AF_INET,
            };
            frame.extend_from_slice(&(family as u32).to_be_bytes());
            frame.extend_from_slice(packet);

            loop {
                let mut guard = self.fd.writable().await?;
                match guard.try_io(|inner| {
                    let n = unsafe {
                        libc::write(
                            inner.as_raw_fd(),
                            frame.as_ptr() as *const libc::c_void,
                            frame.len(),
                        )
                    };
                    if n < 0 {
                        Err(io::Error::last_os_error())
                    } else if n as usize != frame.len() {
                        Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            format!("short write: {n} of {} bytes", frame.len()),
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

        pub fn add_default_route(&self, gateway_ip: Ipv4Addr) -> Result<(), TunError> {
            for cidr in split_default_cidrs() {
                let _ = Command::new("route")
                    .args(["-n", "delete", "-net", cidr, "-interface", &self.name])
                    .status();
                run_command(
                    "route",
                    &["-n", "add", "-net", cidr, "-interface", &self.name],
                    "route add",
                )?;
            }
            debug!(gateway = %gateway_ip, iface = %self.name, "split-default routes added");
            Ok(())
        }

        pub fn add_default_ipv6_route(&self, gateway_ip: Ipv6Addr) -> Result<(), TunError> {
            let gw_str = gateway_ip.to_string();
            for cidr in split_default_ipv6_cidrs() {
                let _ = Command::new("route")
                    .args(["-n", "delete", "-inet6", cidr, "-interface", &self.name])
                    .status();
                run_command(
                    "route",
                    &[
                        "-n",
                        "add",
                        "-inet6",
                        cidr,
                        &gw_str,
                        "-interface",
                        &self.name,
                    ],
                    "route add -inet6",
                )?;
            }
            debug!(gateway = %gateway_ip, iface = %self.name, "split-default IPv6 routes added");
            Ok(())
        }

        pub fn remove_default_route(&self, gateway_ip: Ipv4Addr) -> Result<(), TunError> {
            for cidr in split_default_cidrs() {
                let _ = Command::new("route")
                    .args(["-n", "delete", "-net", cidr, "-interface", &self.name])
                    .status()?;
            }
            debug!(gateway = %gateway_ip, iface = %self.name, "split-default routes removed");
            Ok(())
        }

        pub fn remove_default_ipv6_route(&self, gateway_ip: Ipv6Addr) -> Result<(), TunError> {
            for cidr in split_default_ipv6_cidrs() {
                let _ = Command::new("route")
                    .args(["-n", "delete", "-inet6", cidr, "-interface", &self.name])
                    .status()?;
            }
            debug!(gateway = %gateway_ip, iface = %self.name, "split-default IPv6 routes removed");
            Ok(())
        }
    }

    fn run_command(command: &str, args: &[&str], label: &str) -> Result<(), TunError> {
        let status = Command::new(command).args(args).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(TunError::Ioctl(
                label.into(),
                io::Error::other(format!(
                    "{command} {:?} exited with {:?}",
                    args,
                    status.code()
                )),
            ))
        }
    }

    fn parse_utun_unit(name: &str) -> Result<u32, TunError> {
        let suffix = name
            .strip_prefix("utun")
            .ok_or_else(|| TunError::UnsupportedInterfaceName(name.to_string()))?;
        let unit: u32 = suffix
            .parse()
            .map_err(|_| TunError::UnsupportedInterfaceName(name.to_string()))?;
        Ok(unit + 1)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parse_utun_unit_accepts_numbered_name() {
            assert_eq!(parse_utun_unit("utun0").unwrap(), 1);
            assert_eq!(parse_utun_unit("utun7").unwrap(), 8);
        }

        #[test]
        fn parse_utun_unit_rejects_non_utun_name() {
            assert!(matches!(
                parse_utun_unit("pim0"),
                Err(TunError::UnsupportedInterfaceName(_))
            ));
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::{Ipv4Addr, Ipv6Addr, TunError};

    pub struct PlatformTunInterface;

    impl PlatformTunInterface {
        pub fn create(_name: &str) -> Result<Self, TunError> {
            Err(TunError::Unavailable)
        }

        pub fn name(&self) -> &str {
            ""
        }

        pub fn set_ip(&self, _addr: Ipv4Addr, _prefix_len: u8) -> Result<(), TunError> {
            Err(TunError::Unavailable)
        }

        pub fn set_ipv6(&self, _addr: Ipv6Addr, _prefix_len: u8) -> Result<(), TunError> {
            Err(TunError::Unavailable)
        }

        pub fn set_mtu(&self, _mtu: u32) -> Result<(), TunError> {
            Err(TunError::Unavailable)
        }

        pub fn up(&self) -> Result<(), TunError> {
            Err(TunError::Unavailable)
        }

        pub fn down(&self) -> Result<(), TunError> {
            Err(TunError::Unavailable)
        }

        pub async fn read_packet(&self, _buf: &mut [u8]) -> Result<usize, TunError> {
            Err(TunError::Unavailable)
        }

        pub async fn write_packet(&self, _packet: &[u8]) -> Result<(), TunError> {
            Err(TunError::Unavailable)
        }

        pub fn add_default_route(&self, _gateway_ip: Ipv4Addr) -> Result<(), TunError> {
            Err(TunError::Unavailable)
        }

        pub fn add_default_ipv6_route(&self, _gateway_ip: Ipv6Addr) -> Result<(), TunError> {
            Err(TunError::Unavailable)
        }

        pub fn remove_default_route(&self, _gateway_ip: Ipv4Addr) -> Result<(), TunError> {
            Err(TunError::Unavailable)
        }

        pub fn remove_default_ipv6_route(&self, _gateway_ip: Ipv6Addr) -> Result<(), TunError> {
            Err(TunError::Unavailable)
        }
    }
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
}
