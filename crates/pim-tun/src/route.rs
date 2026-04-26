//! Route helper constants and address utilities.

use std::net::Ipv4Addr;

pub(crate) fn prefix_to_mask(prefix_len: u8) -> Ipv4Addr {
    if prefix_len == 0 {
        return Ipv4Addr::new(0, 0, 0, 0);
    }
    if prefix_len >= 32 {
        return Ipv4Addr::new(255, 255, 255, 255);
    }
    let mask: u32 = !((1u32 << (32 - prefix_len)) - 1);
    Ipv4Addr::from(mask)
}

pub(crate) fn split_default_cidrs() -> [&'static str; 2] {
    ["0.0.0.0/1", "128.0.0.0/1"]
}

pub(crate) fn split_default_ipv6_cidrs() -> [&'static str; 2] {
    ["::/1", "8000::/1"]
}

#[cfg(test)]
mod tests;
