use crate::interface::TunError;
use std::net::{Ipv4Addr, Ipv6Addr};

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
