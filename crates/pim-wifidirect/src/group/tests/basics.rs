use super::super::*;
use crate::wpa_cli::{parse_arp_table, parse_inet_addr};

#[test]
fn group_ip_parsed_from_go_interface() {
    // GO gets 192.168.49.1 assigned by wpa_supplicant.
    let out = "3: p2p-wlan0-0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500\n    link/ether ...\n    inet 192.168.49.1/24 brd 192.168.49.255 scope global p2p-wlan0-0\n";
    let addr = parse_inet_addr(out).unwrap();
    assert_eq!(addr, GO_INTERFACE_IP);
}

#[test]
fn group_ip_parsed_from_gc_interface() {
    // GC gets a DHCP-assigned address in the 192.168.49.0/24 range.
    let out = "3: p2p-wlan0-0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500\n    link/ether ...\n    inet 192.168.49.100/24 brd 192.168.49.255 scope global p2p-wlan0-0\n";
    let addr = parse_inet_addr(out).unwrap();
    assert_eq!(addr, "192.168.49.100".parse::<Ipv4Addr>().unwrap());
}

#[test]
fn group_peer_ip_gc_uses_go_constant() {
    // A GC always knows the GO is at GO_INTERFACE_IP.
    assert_eq!(GO_INTERFACE_IP, "192.168.49.1".parse::<Ipv4Addr>().unwrap());
}

#[test]
fn group_peer_ip_go_reads_arp_table() {
    let content = "IP address       HW type     Flags       HW address            Mask     Device\n192.168.49.100   0x1         0x2         aa:bb:cc:dd:ee:ff     *        p2p-wlan0-0\n";
    let peers = parse_arp_table(content, "p2p-wlan0-0");
    assert_eq!(peers[0], "192.168.49.100".parse::<Ipv4Addr>().unwrap());
}
