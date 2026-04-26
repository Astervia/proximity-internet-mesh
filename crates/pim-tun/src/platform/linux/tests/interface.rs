use super::super::*;

#[test]
#[ignore = "requires CAP_NET_ADMIN"]
fn create_tun_interface() {
    let tun = PlatformTunInterface::create("pim-test0").unwrap();
    assert_eq!(tun.name(), "pim-test0");
}
