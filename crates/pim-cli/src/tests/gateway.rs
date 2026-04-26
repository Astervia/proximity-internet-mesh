use super::super::*;

#[test]
fn gateway_ip_from_static_mesh_cidr_uses_first_host() {
    assert_eq!(
        gateway_ip_from_config_mesh_ip("10.77.0.42/24"),
        Some(Ipv4Addr::new(10, 77, 0, 1))
    );
}

#[test]
fn gateway_ip_from_auto_mesh_ip_is_unknown() {
    assert_eq!(gateway_ip_from_config_mesh_ip("auto"), None);
}

#[test]
#[cfg(target_os = "linux")]
fn route_present_matches_split_default_route() {
    let routes = "0.0.0.0/1 via 10.77.0.1 dev pim0 onlink\n";
    assert!(route_present_linux(
        routes,
        "0.0.0.0/1",
        Ipv4Addr::new(10, 77, 0, 1),
        "pim0"
    ));
}
