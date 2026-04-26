//! TUN interface implementation and platform backends.

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
    inner: crate::platform::PlatformTunInterface,
}

impl TunInterface {
    /// Create a TUN device with the requested name.
    pub fn create(name: &str) -> Result<Self, TunError> {
        Ok(Self {
            inner: crate::platform::PlatformTunInterface::create(name)?,
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
