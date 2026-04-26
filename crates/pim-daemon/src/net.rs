use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};

#[cfg(target_os = "linux")]
pub(crate) struct InternetGatewayLink {
    send_fd_v4: tokio::io::unix::AsyncFd<OwnedFd>,
    send_fd_v6: tokio::io::unix::AsyncFd<OwnedFd>,
    recv_fd_v4: tokio::io::unix::AsyncFd<OwnedFd>,
    recv_fd_v6: tokio::io::unix::AsyncFd<OwnedFd>,
}

#[cfg(target_os = "linux")]
impl InternetGatewayLink {
    pub(crate) fn new(interface: &str) -> Result<Self> {
        let send_fd_v4 = create_raw_send_socket_v4(interface)?;
        let send_fd_v6 = create_raw_send_socket_v6(interface)?;
        let recv_fd_v4 = create_packet_recv_socket_v4(interface)?;
        let recv_fd_v6 = create_packet_recv_socket_v6(interface)?;
        Ok(Self {
            send_fd_v4: tokio::io::unix::AsyncFd::new(send_fd_v4).context("raw send AsyncFd v4")?,
            send_fd_v6: tokio::io::unix::AsyncFd::new(send_fd_v6).context("raw send AsyncFd v6")?,
            recv_fd_v4: tokio::io::unix::AsyncFd::new(recv_fd_v4)
                .context("packet recv AsyncFd v4")?,
            recv_fd_v6: tokio::io::unix::AsyncFd::new(recv_fd_v6)
                .context("packet recv AsyncFd v6")?,
        })
    }

    pub(crate) async fn send_packet(&self, packet: &[u8]) -> Result<()> {
        match packet_ip_version(packet) {
            Some(4) => {
                let dest_ip = ipv4_destination(packet).context("raw send requires IPv4 packet")?;
                let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
                addr.sin_family = libc::AF_INET as u16;
                addr.sin_addr = libc::in_addr {
                    s_addr: u32::from(dest_ip).to_be(),
                };

                loop {
                    let mut guard = self
                        .send_fd_v4
                        .writable()
                        .await
                        .context("raw send socket writable v4")?;
                    match guard.try_io(|inner| {
                        let rc = unsafe {
                            libc::sendto(
                                inner.as_raw_fd(),
                                packet.as_ptr() as *const libc::c_void,
                                packet.len(),
                                0,
                                &addr as *const _ as *const libc::sockaddr,
                                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                            )
                        };
                        if rc < 0 {
                            Err(io::Error::last_os_error())
                        } else {
                            Ok(())
                        }
                    }) {
                        Ok(result) => return result.context("raw sendto failed v4"),
                        Err(_would_block) => continue,
                    }
                }
            }
            Some(6) => {
                let dest_ip = ipv6_destination(packet).context("raw send requires IPv6 packet")?;
                let mut addr: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
                addr.sin6_family = libc::AF_INET6 as u16;
                addr.sin6_addr = libc::in6_addr {
                    s6_addr: dest_ip.octets(),
                };

                loop {
                    let mut guard = self
                        .send_fd_v6
                        .writable()
                        .await
                        .context("raw send socket writable v6")?;
                    match guard.try_io(|inner| {
                        let rc = unsafe {
                            libc::sendto(
                                inner.as_raw_fd(),
                                packet.as_ptr() as *const libc::c_void,
                                packet.len(),
                                0,
                                &addr as *const _ as *const libc::sockaddr,
                                std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                            )
                        };
                        if rc < 0 {
                            Err(io::Error::last_os_error())
                        } else {
                            Ok(())
                        }
                    }) {
                        Ok(result) => return result.context("raw sendto failed v6"),
                        Err(_would_block) => continue,
                    }
                }
            }
            _ => bail!("raw send requires IPv4 or IPv6 packet"),
        }
    }

    pub(crate) async fn recv_packet(&self, buf: &mut [u8]) -> Result<usize> {
        tokio::select! {
            result = recv_linux_packet_owned(&self.recv_fd_v4, buf.len(), "packet recv v4") => {
                let (packet, size) = result?;
                buf[..size].copy_from_slice(&packet[..size]);
                Ok(size)
            },
            result = recv_linux_packet_owned(&self.recv_fd_v6, buf.len(), "packet recv v6") => {
                let (packet, size) = result?;
                buf[..size].copy_from_slice(&packet[..size]);
                Ok(size)
            },
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) struct InternetGatewayLink {
    send_fd: tokio::io::unix::AsyncFd<OwnedFd>,
    recv_icmp_fd: tokio::io::unix::AsyncFd<OwnedFd>,
    recv_tcp_fd: tokio::io::unix::AsyncFd<OwnedFd>,
    recv_udp_fd: tokio::io::unix::AsyncFd<OwnedFd>,
}

#[cfg(target_os = "macos")]
impl InternetGatewayLink {
    pub(crate) fn new(interface: &str) -> Result<Self> {
        let send_fd = create_raw_send_socket(interface)?;
        let recv_icmp_fd = create_raw_recv_socket(interface, libc::IPPROTO_ICMP)?;
        let recv_tcp_fd = create_raw_recv_socket(interface, libc::IPPROTO_TCP)?;
        let recv_udp_fd = create_raw_recv_socket(interface, libc::IPPROTO_UDP)?;
        Ok(Self {
            send_fd: tokio::io::unix::AsyncFd::new(send_fd).context("raw send AsyncFd")?,
            recv_icmp_fd: tokio::io::unix::AsyncFd::new(recv_icmp_fd)
                .context("icmp recv AsyncFd")?,
            recv_tcp_fd: tokio::io::unix::AsyncFd::new(recv_tcp_fd).context("tcp recv AsyncFd")?,
            recv_udp_fd: tokio::io::unix::AsyncFd::new(recv_udp_fd).context("udp recv AsyncFd")?,
        })
    }

    pub(crate) async fn send_packet(&self, packet: &[u8]) -> Result<()> {
        let dest_ip = ipv4_destination(packet).context("raw send requires IPv4 packet")?;
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        addr.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
        addr.sin_family = libc::AF_INET as u8;
        addr.sin_addr = libc::in_addr {
            s_addr: u32::from(dest_ip).to_be(),
        };

        loop {
            let mut guard = self
                .send_fd
                .writable()
                .await
                .context("raw send socket writable")?;
            match guard.try_io(|inner| {
                let rc = unsafe {
                    libc::sendto(
                        inner.as_raw_fd(),
                        packet.as_ptr() as *const libc::c_void,
                        packet.len(),
                        0,
                        &addr as *const _ as *const libc::sockaddr,
                        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                    )
                };
                if rc < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }) {
                Ok(result) => return result.context("raw sendto failed"),
                Err(_would_block) => continue,
            }
        }
    }

    pub(crate) async fn recv_packet(&self, buf: &mut [u8]) -> Result<usize> {
        tokio::select! {
            result = recv_raw_protocol_packet(&self.recv_icmp_fd, buf.len(), "icmp recv") => {
                let (packet, size) = result?;
                buf[..size].copy_from_slice(&packet[..size]);
                Ok(size)
            }
            result = recv_raw_protocol_packet(&self.recv_tcp_fd, buf.len(), "tcp recv") => {
                let (packet, size) = result?;
                buf[..size].copy_from_slice(&packet[..size]);
                Ok(size)
            }
            result = recv_raw_protocol_packet(&self.recv_udp_fd, buf.len(), "udp recv") => {
                let (packet, size) = result?;
                buf[..size].copy_from_slice(&packet[..size]);
                Ok(size)
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) struct InternetGatewayLink;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl InternetGatewayLink {
    pub(crate) fn new(_interface: &str) -> Result<Self> {
        bail!("gateway internet link is only supported on Linux and macOS")
    }

    pub(crate) async fn send_packet(&self, _packet: &[u8]) -> Result<()> {
        bail!("gateway internet link is only supported on Linux and macOS")
    }

    pub(crate) async fn recv_packet(&self, _buf: &mut [u8]) -> Result<usize> {
        bail!("gateway internet link is only supported on Linux and macOS")
    }
}

pub(crate) fn packet_ip_version(packet: &[u8]) -> Option<u8> {
    packet.first().map(|byte| byte >> 4)
}

pub(crate) fn ipv4_destination(packet: &[u8]) -> Option<Ipv4Addr> {
    if packet.len() < 20 || (packet[0] >> 4) != 4 {
        return None;
    }
    Some(Ipv4Addr::new(
        packet[16], packet[17], packet[18], packet[19],
    ))
}

pub(crate) fn ipv6_destination(packet: &[u8]) -> Option<Ipv6Addr> {
    if packet.len() < 40 || (packet[0] >> 4) != 6 {
        return None;
    }
    let mut octets = [0u8; 16];
    octets.copy_from_slice(&packet[24..40]);
    Some(Ipv6Addr::from(octets))
}

pub(crate) fn lookup_interface_ipv4(interface: &str) -> Result<Ipv4Addr> {
    #[cfg(target_os = "linux")]
    let output = Command::new("ip")
        .args(["-4", "-o", "addr", "show", "dev", interface])
        .output()
        .with_context(|| format!("failed to inspect IPv4 address for {interface}"))?;
    #[cfg(target_os = "macos")]
    let output = Command::new("ifconfig")
        .arg(interface)
        .output()
        .with_context(|| format!("failed to inspect IPv4 address for {interface}"))?;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    bail!("interface IPv4 lookup is not supported on this platform");

    if !output.status.success() {
        #[cfg(target_os = "linux")]
        bail!(
            "ip -4 -o addr show dev {interface} exited with {:?}",
            output.status.code()
        );
        #[cfg(target_os = "macos")]
        bail!(
            "ifconfig {interface} exited with {:?}",
            output.status.code()
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        unreachable!();
    }

    let stdout = String::from_utf8(output.stdout).context("invalid UTF-8 from ip addr output")?;
    parse_interface_ipv4_output(&stdout)
        .with_context(|| format!("no IPv4 address found on interface {interface}"))
}

/// Scan all interfaces (except the mesh TUN) for any global IPv6 address. Used
/// as a fallback when the configured `nat_interface` has no global IPv6 —
/// common in Docker test environments where interface→network mapping
/// (eth0 vs eth1) is non-deterministic even though the YAML declares an order.
#[cfg(target_os = "linux")]
pub(crate) fn find_any_ipv6_uplink(exclude: &[&str]) -> Option<(String, Ipv6Addr)> {
    let output = Command::new("ip")
        .args(["-6", "-o", "addr", "show", "scope", "global"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    for line in stdout.lines() {
        let mut it = line.split_whitespace();
        let _idx = it.next();
        let iface = it.next()?;
        if exclude.contains(&iface) {
            continue;
        }
        let _fam = it.next();
        let addr_cidr = it.next()?;
        let ip_str = addr_cidr.split('/').next()?;
        if let Ok(ip) = ip_str.parse::<Ipv6Addr>() {
            return Some((iface.to_string(), ip));
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn find_any_ipv6_uplink(_exclude: &[&str]) -> Option<(String, Ipv6Addr)> {
    None
}

pub(crate) fn lookup_interface_ipv6(interface: &str) -> Result<Ipv6Addr> {
    #[cfg(target_os = "linux")]
    let output = Command::new("ip")
        .args([
            "-6", "-o", "addr", "show", "scope", "global", "dev", interface,
        ])
        .output()
        .with_context(|| format!("failed to inspect IPv6 address for {interface}"))?;
    #[cfg(target_os = "macos")]
    let output = Command::new("ifconfig")
        .arg(interface)
        .output()
        .with_context(|| format!("failed to inspect IPv6 address for {interface}"))?;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    bail!("interface IPv6 lookup is not supported on this platform");

    if !output.status.success() {
        #[cfg(target_os = "linux")]
        bail!(
            "ip -6 -o addr show scope global dev {interface} exited with {:?}",
            output.status.code()
        );
        #[cfg(target_os = "macos")]
        bail!(
            "ifconfig {interface} exited with {:?}",
            output.status.code()
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        unreachable!();
    }

    let stdout =
        String::from_utf8(output.stdout).context("invalid UTF-8 from interface IPv6 output")?;
    parse_interface_ipv6_output(&stdout)
        .with_context(|| format!("no global IPv6 address found on interface {interface}"))
}

pub(crate) async fn lookup_interface_ipv6_with_retry(interface: &str) -> Result<Ipv6Addr> {
    // Short retry budget: a statically-addressed container has IPv6 immediately,
    // and a SLAAC'd interface finishes within ~2s. On IPv4-only containers this
    // path would otherwise block the whole startup until retries are exhausted.
    let mut last_err = None;
    for _ in 0..8 {
        match lookup_interface_ipv6(interface) {
            Ok(ip) => return Ok(ip),
            Err(err) => last_err = Some(err),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("failed to resolve IPv6 address")))
}

pub(crate) fn parse_interface_ipv4_output(output: &str) -> Option<Ipv4Addr> {
    output
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|w| (w[0] == "inet").then_some(w[1]))
        .and_then(|token| token.split('/').next())
        .and_then(|ip| ip.parse().ok())
}

pub(crate) fn parse_interface_ipv6_output(output: &str) -> Option<Ipv6Addr> {
    output
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|window| (window[0] == "inet6").then_some(window[1]))
        .and_then(|token| token.split('/').next())
        .and_then(|ip| ip.split('%').next())
        .and_then(|ip| ip.parse().ok())
}

#[cfg(target_os = "linux")]
fn create_raw_send_socket_v4(interface: &str) -> Result<OwnedFd> {
    let fd = unsafe {
        libc::socket(
            libc::AF_INET,
            libc::SOCK_RAW | libc::SOCK_NONBLOCK,
            libc::IPPROTO_RAW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("create raw send socket");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let one: libc::c_int = 1;
    setsockopt_bytes(
        fd.as_raw_fd(),
        libc::IPPROTO_IP,
        libc::IP_HDRINCL,
        &one.to_ne_bytes(),
    )
    .context("set IP_HDRINCL")?;
    bind_socket_to_device(fd.as_raw_fd(), interface).context("bind raw send socket to device")?;
    Ok(fd)
}

#[cfg(target_os = "macos")]
fn create_raw_send_socket(interface: &str) -> Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_RAW) };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("create raw send socket");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    set_nonblocking(fd.as_raw_fd()).context("set raw send socket nonblocking")?;
    let one: libc::c_int = 1;
    setsockopt_bytes(
        fd.as_raw_fd(),
        libc::IPPROTO_IP,
        libc::IP_HDRINCL,
        &one.to_ne_bytes(),
    )
    .context("set IP_HDRINCL")?;
    bind_socket_to_interface(fd.as_raw_fd(), interface)
        .context("bind raw send socket to device")?;
    Ok(fd)
}

#[cfg(target_os = "linux")]
fn create_raw_send_socket_v6(interface: &str) -> Result<OwnedFd> {
    let fd = unsafe {
        libc::socket(
            libc::AF_INET6,
            libc::SOCK_RAW | libc::SOCK_NONBLOCK,
            libc::IPPROTO_RAW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("create raw send socket v6");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let one: libc::c_int = 1;
    setsockopt_bytes(
        fd.as_raw_fd(),
        libc::IPPROTO_IPV6,
        libc::IPV6_HDRINCL,
        &one.to_ne_bytes(),
    )
    .context("set IPV6_HDRINCL")?;
    bind_socket_to_device(fd.as_raw_fd(), interface)
        .context("bind raw send socket v6 to device")?;
    Ok(fd)
}

#[cfg(target_os = "linux")]
fn create_packet_recv_socket_v4(interface: &str) -> Result<OwnedFd> {
    const ETH_P_IP: u16 = 0x0800;
    let fd = unsafe {
        libc::socket(
            libc::AF_PACKET,
            libc::SOCK_DGRAM | libc::SOCK_NONBLOCK,
            i32::from(ETH_P_IP.to_be()),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("create packet recv socket");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    bind_socket_to_device(fd.as_raw_fd(), interface).context("bind packet socket to device")?;

    let if_name = std::ffi::CString::new(interface).context("interface contains interior NUL")?;
    let ifindex = unsafe { libc::if_nametoindex(if_name.as_ptr()) };
    if ifindex == 0 {
        return Err(io::Error::last_os_error()).context("if_nametoindex failed");
    }

    let mut addr: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
    addr.sll_family = libc::AF_PACKET as u16;
    addr.sll_protocol = ETH_P_IP.to_be();
    addr.sll_ifindex = ifindex as i32;
    let rc = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error()).context("bind packet socket");
    }
    Ok(fd)
}

#[cfg(target_os = "linux")]
fn create_packet_recv_socket_v6(interface: &str) -> Result<OwnedFd> {
    const ETH_P_IPV6: u16 = 0x86dd;
    let fd = unsafe {
        libc::socket(
            libc::AF_PACKET,
            libc::SOCK_DGRAM | libc::SOCK_NONBLOCK,
            i32::from(ETH_P_IPV6.to_be()),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("create packet recv socket v6");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    bind_socket_to_device(fd.as_raw_fd(), interface).context("bind packet socket v6 to device")?;

    let if_name = std::ffi::CString::new(interface).context("interface contains interior NUL")?;
    let ifindex = unsafe { libc::if_nametoindex(if_name.as_ptr()) };
    if ifindex == 0 {
        return Err(io::Error::last_os_error()).context("if_nametoindex failed");
    }

    let mut addr: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
    addr.sll_family = libc::AF_PACKET as u16;
    addr.sll_protocol = ETH_P_IPV6.to_be();
    addr.sll_ifindex = ifindex as i32;
    let rc = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error()).context("bind packet socket v6");
    }
    Ok(fd)
}

#[cfg(target_os = "linux")]
fn bind_socket_to_device(fd: i32, interface: &str) -> Result<()> {
    let mut name = interface.as_bytes().to_vec();
    name.push(0);
    setsockopt_bytes(fd, libc::SOL_SOCKET, libc::SO_BINDTODEVICE, &name)
        .with_context(|| format!("SO_BINDTODEVICE failed for {interface}"))
}

#[cfg(target_os = "linux")]
async fn recv_linux_packet_owned(
    fd: &tokio::io::unix::AsyncFd<OwnedFd>,
    buf_len: usize,
    context: &'static str,
) -> Result<(Vec<u8>, usize)> {
    let mut packet = vec![0u8; buf_len];
    loop {
        let mut guard = fd
            .readable()
            .await
            .with_context(|| format!("{context} readable"))?;
        match guard.try_io(|inner| {
            let rc = unsafe {
                libc::recv(
                    inner.as_raw_fd(),
                    packet.as_mut_ptr() as *mut libc::c_void,
                    packet.len(),
                    0,
                )
            };
            if rc < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(rc as usize)
            }
        }) {
            Ok(result) => return result.map(|size| (packet, size)).context(context),
            Err(_would_block) => continue,
        }
    }
}

#[cfg(target_os = "macos")]
fn create_raw_recv_socket(interface: &str, protocol: i32) -> Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_RAW, protocol) };
    if fd < 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("create raw recv socket for protocol {protocol}"));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    set_nonblocking(fd.as_raw_fd()).context("set raw recv socket nonblocking")?;
    bind_socket_to_interface(fd.as_raw_fd(), interface)
        .context("bind raw recv socket to device")?;
    Ok(fd)
}

#[cfg(target_os = "macos")]
const IP_BOUND_IF: libc::c_int = 25;

#[cfg(target_os = "macos")]
fn bind_socket_to_interface(fd: i32, interface: &str) -> Result<()> {
    let if_name = std::ffi::CString::new(interface).context("interface contains interior NUL")?;
    let ifindex = unsafe { libc::if_nametoindex(if_name.as_ptr()) };
    if ifindex == 0 {
        return Err(io::Error::last_os_error()).context("if_nametoindex failed");
    }

    let ifindex = (ifindex as libc::c_uint).to_ne_bytes();
    setsockopt_bytes(fd, libc::IPPROTO_IP, IP_BOUND_IF, &ifindex)
        .with_context(|| format!("IP_BOUND_IF failed for {interface}"))
}

#[cfg(target_os = "macos")]
async fn recv_raw_protocol_packet(
    fd: &tokio::io::unix::AsyncFd<OwnedFd>,
    buf_len: usize,
    context: &'static str,
) -> Result<(Vec<u8>, usize)> {
    let mut packet = vec![0u8; buf_len];
    loop {
        let mut guard = fd
            .readable()
            .await
            .with_context(|| format!("{context} readable"))?;
        match guard.try_io(|inner| {
            let rc = unsafe {
                libc::recv(
                    inner.as_raw_fd(),
                    packet.as_mut_ptr() as *mut libc::c_void,
                    packet.len(),
                    0,
                )
            };
            if rc < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(rc as usize)
            }
        }) {
            Ok(result) => return result.map(|size| (packet, size)).context(context),
            Err(_would_block) => continue,
        }
    }
}

#[cfg(target_os = "macos")]
fn set_nonblocking(fd: i32) -> Result<()> {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(io::Error::last_os_error()).context("fcntl(F_GETFL) failed");
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error()).context("fcntl(F_SETFL) failed");
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn setsockopt_bytes(fd: i32, level: i32, optname: i32, value: &[u8]) -> Result<()> {
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            optname,
            value.as_ptr() as *const libc::c_void,
            value.len() as libc::socklen_t,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error()).context("setsockopt failed")
    } else {
        Ok(())
    }
}
