use crate::interface::TunError;
use crate::route::{prefix_to_mask, split_default_cidrs, split_default_ipv6_cidrs};
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
        let _ = gateway_ip;
        for cidr in split_default_ipv6_cidrs() {
            let status = std::process::Command::new("ip")
                .args(["-6", "route", "replace", cidr, "dev", &self.name])
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
        let _ = gateway_ip;
        for cidr in split_default_ipv6_cidrs() {
            let _ = std::process::Command::new("ip")
                .args(["-6", "route", "del", cidr, "dev", &self.name])
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
