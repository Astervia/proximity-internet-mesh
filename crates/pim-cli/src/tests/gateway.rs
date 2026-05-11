use super::super::*;

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
