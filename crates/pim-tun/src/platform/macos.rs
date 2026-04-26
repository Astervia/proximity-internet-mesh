use crate::interface::TunError;
use crate::route::{prefix_to_mask, split_default_cidrs, split_default_ipv6_cidrs};
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
        let fd = unsafe { libc::socket(libc::PF_SYSTEM, libc::SOCK_DGRAM, libc::SYSPROTO_CONTROL) };
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
        let _ = gateway_ip;
        for cidr in split_default_ipv6_cidrs() {
            let _ = Command::new("route")
                .args(["-n", "delete", "-inet6", cidr, "-interface", &self.name])
                .status();
            run_command(
                "route",
                &["-n", "add", "-inet6", cidr, "-interface", &self.name],
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
mod tests;
