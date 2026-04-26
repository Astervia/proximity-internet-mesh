use super::super::*;

#[test]
fn operstate_helper_accepts_up_and_unknown() {
    assert!(is_ready_operstate("up\n"));
    assert!(is_ready_operstate("unknown"));
    assert!(!is_ready_operstate("down"));
}

#[test]
fn ifconfig_output_treats_non_inactive_interface_as_ready() {
    let active = "\
bridge0: flags=41<UP,RUNNING> mtu 1500\n\
\tstatus: active\n";
    let no_status = "bridge0: flags=41<UP,RUNNING> mtu 1500\n";
    let inactive = "\
bridge0: flags=41<UP,RUNNING> mtu 1500\n\
\tstatus: inactive\n";
    assert!(is_ready_ifconfig_output(active));
    assert!(is_ready_ifconfig_output(no_status));
    assert!(!is_ready_ifconfig_output(inactive));
}
#[test]
fn neighbor_output_parses_ipv4_and_ipv6_and_skips_failed_entries() {
    let output = "\
192.168.44.2 dev bnep0 lladdr 02:00:00:00:00:02 REACHABLE
fe80::1234 dev bnep0 lladdr 02:00:00:00:00:03 router STALE
192.168.44.3 dev bnep0 FAILED
";
    let parsed = parse_neighbor_output(output, 9100, Some(7));
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0], "192.168.44.2:9100".parse().unwrap());
    assert_eq!(
        parsed[1],
        SocketAddr::V6(SocketAddrV6::new("fe80::1234".parse().unwrap(), 9100, 0, 7))
    );
}

#[test]
fn devices_output_filters_to_matching_prefix() {
    let output = "\
Device AA:BB:CC:DD:EE:01 PIM-gateway
Device AA:BB:CC:DD:EE:02 Phone
Device AA:BB:CC:DD:EE:03 PIM-client
";
    let parsed = parse_devices_output(output, "PIM-", "PIM-self");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].mac, "AA:BB:CC:DD:EE:01");
    assert_eq!(parsed[1].name, "PIM-client");
}

#[test]
fn blueutil_inquiry_output_filters_to_matching_prefix() {
    let output = "\
address: aa-bb-cc-dd-ee-01, not connected, not favourite, not paired, name: \"PIM-gateway\", recent access date: -\n\
address: aa-bb-cc-dd-ee-02, not connected, not favourite, not paired, name: \"Phone\", recent access date: -\n\
address: aa-bb-cc-dd-ee-03, not connected, not favourite, not paired, name: \"PIM-self\", recent access date: -\n";
    let parsed = parse_blueutil_inquiry_output(output, "PIM-", "PIM-self");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].mac, "AA:BB:CC:DD:EE:01");
    assert_eq!(parsed[0].name, "PIM-gateway");
}

#[test]
fn arp_output_parses_interface_scoped_neighbors() {
    let output = "\
? (192.168.44.2) at aa:bb:cc:dd:ee:01 on bridge0 ifscope [ethernet]\n\
? (192.168.44.3) at aa:bb:cc:dd:ee:02 on en0 ifscope [ethernet]\n\
? (fe80::1) at aa:bb:cc:dd:ee:03 on bridge0 ifscope permanent [ethernet]\n";
    let parsed = parse_arp_output(output, "bridge0", 9100);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0], "192.168.44.2:9100".parse().unwrap());
    assert_eq!(parsed[1], "[fe80::1]:9100".parse().unwrap());
}
#[test]
fn parse_ipv4_cidr_accepts_common_cases() {
    let (ip, prefix) = parse_ipv4_cidr("192.168.44.1/24").unwrap();
    assert_eq!(ip, std::net::Ipv4Addr::new(192, 168, 44, 1));
    assert_eq!(prefix, 24);
    assert!(parse_ipv4_cidr("not a cidr").is_err());
    assert!(parse_ipv4_cidr("192.168.44.1/33").is_err());
}

#[test]
fn default_dhcp_range_keeps_gateway_out_of_pool() {
    let range = default_dhcp_range(std::net::Ipv4Addr::new(192, 168, 44, 1), 24).unwrap();
    assert_eq!(range, "192.168.44.10,192.168.44.245");
}
