use super::super::*;

#[test]
fn parse_interface_ipv4_output_extracts_first_inet_cidr() {
    let output = "2: eno1    inet 192.168.0.137/24 brd 192.168.0.255 scope global dynamic eno1\n";
    assert_eq!(
        parse_interface_ipv4_output(output),
        Some(Ipv4Addr::new(192, 168, 0, 137))
    );
}

#[test]
fn parse_interface_ipv4_output_extracts_macos_ifconfig_inet() {
    let output = "\
en0: flags=8863<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n\
\tinet 192.168.1.44 netmask 0xffffff00 broadcast 192.168.1.255\n\
\tinet6 fe80::1234%en0 prefixlen 64 secured scopeid 0x4\n";
    assert_eq!(
        parse_interface_ipv4_output(output),
        Some(Ipv4Addr::new(192, 168, 1, 44))
    );
}

#[test]
fn ipv4_destination_reads_destination_octets() {
    let packet = [
        0x45, 0, 0, 20, 0, 0, 0, 0, 64, 1, 0, 0, 10, 77, 0, 2, 1, 1, 1, 1,
    ];
    assert_eq!(ipv4_destination(&packet), Some(Ipv4Addr::new(1, 1, 1, 1)));
}
