use super::super::*;

#[test]
fn wpa_cli_p2p_peers_parses_empty_output() {
    let peers = parse_p2p_peers("");
    assert!(peers.is_empty());
}

#[test]
fn wpa_cli_p2p_peers_parses_single_mac() {
    let out = "aa:bb:cc:dd:ee:ff\n";
    let peers = parse_p2p_peers(out);
    assert_eq!(peers, vec!["aa:bb:cc:dd:ee:ff"]);
}

#[test]
fn wpa_cli_p2p_peers_parses_multiple_macs() {
    let out = "aa:bb:cc:dd:ee:ff\n11:22:33:44:55:66\n";
    let peers = parse_p2p_peers(out);
    assert_eq!(peers.len(), 2);
    assert!(peers.contains(&"aa:bb:cc:dd:ee:ff".to_string()));
    assert!(peers.contains(&"11:22:33:44:55:66".to_string()));
}

#[test]
fn wpa_cli_p2p_peers_ignores_header_lines() {
    let out = "Selected interface 'wlan0'\naa:bb:cc:dd:ee:ff\n";
    let peers = parse_p2p_peers(out);
    assert_eq!(peers, vec!["aa:bb:cc:dd:ee:ff"]);
}

#[test]
fn p2p_peer_info_parses_device_name() {
    let out = "aa:bb:cc:dd:ee:ff\ndevice_name=MyPhone\npri_dev_type=10-0050F204-5\nconfig_methods=0x0188\n";
    let info = parse_p2p_peer_info("aa:bb:cc:dd:ee:ff", out).unwrap();
    assert_eq!(info.device_name, "MyPhone");
    assert_eq!(info.pri_dev_type, "10-0050F204-5");
    assert_eq!(info.config_methods, 0x0188);
}

#[test]
fn p2p_peer_info_returns_error_when_device_name_missing() {
    let out = "aa:bb:cc:dd:ee:ff\npri_dev_type=10-0050F204-5\n";
    assert!(parse_p2p_peer_info("aa:bb:cc:dd:ee:ff", out).is_err());
}

#[test]
fn parse_inet_addr_extracts_ip() {
    let out = "2: p2p-wlan0-0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500\n    link/ether aa:bb:cc:dd:ee:ff brd ff:ff:ff:ff:ff:ff\n    inet 192.168.49.1/24 brd 192.168.49.255 scope global p2p-wlan0-0\n";
    let addr = parse_inet_addr(out).unwrap();
    assert_eq!(addr, "192.168.49.1".parse::<Ipv4Addr>().unwrap());
}

#[test]
fn parse_inet_addr_returns_none_for_no_ip() {
    let out = "2: p2p-wlan0-0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500\n";
    assert!(parse_inet_addr(out).is_none());
}

#[test]
fn parse_arp_table_extracts_peer_on_iface() {
    let content = "IP address       HW type     Flags       HW address            Mask     Device\n192.168.49.100   0x1         0x2         aa:bb:cc:dd:ee:ff     *        p2p-wlan0-0\n10.0.0.1         0x1         0x2         11:22:33:44:55:66     *        eth0\n";
    let peers = parse_arp_table(content, "p2p-wlan0-0");
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0], "192.168.49.100".parse::<Ipv4Addr>().unwrap());
}

#[test]
fn parse_arp_table_returns_empty_for_wrong_iface() {
    let content = "IP address       HW type     Flags       HW address            Mask     Device\n192.168.49.100   0x1         0x2         aa:bb:cc:dd:ee:ff     *        p2p-wlan0-0\n";
    let peers = parse_arp_table(content, "eth0");
    assert!(peers.is_empty());
}

#[test]
fn parse_interface_list_extracts_interfaces() {
    let out = "Available interfaces:\nwlan0\np2p-wlan0-0\n";
    let ifaces = parse_interface_list(out);
    assert!(ifaces.contains(&"wlan0".to_string()));
    assert!(ifaces.contains(&"p2p-wlan0-0".to_string()));
}
