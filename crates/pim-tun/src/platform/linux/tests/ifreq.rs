use super::super::*;

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
