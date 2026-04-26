use super::super::*;

#[cfg(target_os = "linux")]
#[test]
fn input_drop_args_for_tcp_cover_reserved_nat_range() {
    assert_eq!(
        input_drop_args("tcp", "eno1"),
        [
            "-A",
            "INPUT",
            "-i",
            "eno1",
            "-p",
            "tcp",
            "--dport",
            "30000:59999",
            "-j",
            "DROP"
        ]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn input_drop_args_for_udp_cover_reserved_nat_range() {
    assert_eq!(
        input_drop_args("udp", "eno1"),
        [
            "-A",
            "INPUT",
            "-i",
            "eno1",
            "-p",
            "udp",
            "--dport",
            "30000:59999",
            "-j",
            "DROP"
        ]
    );
}
