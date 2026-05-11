use super::*;

fn nid(byte: u8) -> NodeId {
    NodeId::from_bytes([byte; 16])
}

fn nid_from(bytes: [u8; 16]) -> NodeId {
    NodeId::from_bytes(bytes)
}

#[test]
fn ipv4_prefix_zeroes_host_bits() {
    let p = Ipv4Prefix::parse("10.77.5.42/16").unwrap();
    assert_eq!(p.network, Ipv4Addr::new(10, 77, 0, 0));
    assert_eq!(p.prefix_len, 16);
}

#[test]
fn ipv6_prefix_zeroes_host_bits() {
    let p = Ipv6Prefix::parse("fd77:0:0:0:1:2:3:4/64").unwrap();
    assert_eq!(p.network, Ipv6Addr::new(0xfd77, 0, 0, 0, 0, 0, 0, 0));
    assert_eq!(p.prefix_len, 64);
}

#[test]
fn invalid_cidr_strings_rejected() {
    assert!(Ipv4Prefix::parse("10.77.0.0").is_err());
    assert!(Ipv4Prefix::parse("10.77.0.0/33").is_err());
    assert!(Ipv4Prefix::parse("not-an-ip/16").is_err());
    assert!(Ipv6Prefix::parse("fd77::/129").is_err());
    assert!(Ipv6Prefix::parse("not-an-ip/64").is_err());
}

#[test]
fn ipv4_derivation_is_deterministic() {
    let prefix = Ipv4Prefix::parse(DEFAULT_MESH_IPV4_PREFIX).unwrap();
    let id = nid(0x42);
    assert_eq!(derive_mesh_ipv4(&id, prefix), derive_mesh_ipv4(&id, prefix));
}

#[test]
fn ipv6_derivation_is_deterministic() {
    let prefix = Ipv6Prefix::parse(DEFAULT_MESH_IPV6_PREFIX).unwrap();
    let id = nid(0x42);
    assert_eq!(derive_mesh_ipv6(&id, prefix), derive_mesh_ipv6(&id, prefix));
}

#[test]
fn ipv4_derivation_stays_inside_prefix() {
    let prefix = Ipv4Prefix::parse("10.77.0.0/16").unwrap();
    for byte in 0u8..=255 {
        let ip = derive_mesh_ipv4(&nid(byte), prefix);
        let octets = ip.octets();
        assert_eq!(&octets[..2], &[10, 77], "byte {byte}: {ip}");
    }
}

#[test]
fn ipv6_derivation_stays_inside_prefix() {
    let prefix = Ipv6Prefix::parse("fd77::/64").unwrap();
    for byte in 0u8..=255 {
        let ip = derive_mesh_ipv6(&nid(byte), prefix);
        let segs = ip.segments();
        assert_eq!(segs[0], 0xfd77, "byte {byte}: {ip}");
        assert_eq!(&segs[1..4], &[0, 0, 0]);
    }
}

#[test]
fn ipv4_derivation_skips_reserved_hosts() {
    // Use a /30 (4 addresses, 2 usable) — small enough that the
    // derivation must collide with reserved hosts repeatedly. We just
    // assert host is never 0 or all-ones.
    let prefix = Ipv4Prefix::parse("192.0.2.0/30").unwrap();
    for byte in 0u8..=255 {
        let ip = derive_mesh_ipv4(&nid(byte), prefix);
        let last = u32::from(ip) & 0b11;
        assert!(
            last == 1 || last == 2,
            "byte {byte}: {ip} hit a reserved host bit"
        );
    }
}

#[test]
fn ipv6_derivation_skips_reserved_hosts() {
    let prefix = Ipv6Prefix::parse("fd77::/126").unwrap();
    for byte in 0u8..=255 {
        let ip = derive_mesh_ipv6(&nid(byte), prefix);
        let last = u128::from(ip) & 0b11;
        assert!(
            last == 1 || last == 2,
            "byte {byte}: {ip} hit a reserved host bit"
        );
    }
}

#[test]
fn flipping_a_byte_changes_ipv4_address() {
    let prefix = Ipv4Prefix::parse("10.77.0.0/16").unwrap();
    let mut bytes = [0u8; 16];
    let base = derive_mesh_ipv4(&nid_from(bytes), prefix);
    let mut flipped_count = 0usize;
    for i in 0..16 {
        bytes[i] ^= 0x01;
        if derive_mesh_ipv4(&nid_from(bytes), prefix) != base {
            flipped_count += 1;
        }
        bytes[i] ^= 0x01;
    }
    // With 16-bit host space and a strong hash, almost every flip
    // should change the host bits. Allow a small slack so the test
    // remains stable across blake3 minor versions.
    assert!(
        flipped_count >= 14,
        "expected most NodeId byte flips to perturb the IPv4 derivation, got {flipped_count}/16"
    );
}

#[test]
fn flipping_a_byte_changes_ipv6_address() {
    let prefix = Ipv6Prefix::parse("fd77::/64").unwrap();
    let mut bytes = [0u8; 16];
    let base = derive_mesh_ipv6(&nid_from(bytes), prefix);
    for i in 0..16 {
        bytes[i] ^= 0x01;
        assert_ne!(
            derive_mesh_ipv6(&nid_from(bytes), prefix),
            base,
            "flipping byte {i} produced an identical IPv6 derivation"
        );
        bytes[i] ^= 0x01;
    }
}

#[test]
fn verify_round_trips() {
    let prefix4 = Ipv4Prefix::parse("10.77.0.0/16").unwrap();
    let prefix6 = Ipv6Prefix::parse("fd77::/64").unwrap();
    let id = nid(0x99);
    let ip4 = derive_mesh_ipv4(&id, prefix4);
    let ip6 = derive_mesh_ipv6(&id, prefix6);
    assert!(verify_mesh_ipv4(&id, ip4, prefix4));
    assert!(verify_mesh_ipv6(&id, ip6, prefix6));
    let other = nid(0x9A);
    assert!(!verify_mesh_ipv4(&other, ip4, prefix4));
    assert!(!verify_mesh_ipv6(&other, ip6, prefix6));
}

#[test]
fn v4_and_v6_streams_are_independent() {
    // Different domain-separating contexts must produce independent
    // bit streams — the low bytes of the v4 derivation should not
    // simply mirror the low bytes of the v6 derivation.
    let prefix4 = Ipv4Prefix::parse("10.77.0.0/16").unwrap();
    let prefix6 = Ipv6Prefix::parse("fd77::/64").unwrap();
    let id = nid(0x42);
    let ip4 = derive_mesh_ipv4(&id, prefix4);
    let ip6 = derive_mesh_ipv6(&id, prefix6);
    let host4 = u32::from(ip4) & 0xffff;
    let host6_low = (u128::from(ip6) & 0xffff) as u32;
    assert_ne!(host4, host6_low);
}
