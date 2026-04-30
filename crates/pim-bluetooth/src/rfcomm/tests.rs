//! Cross-platform unit tests for the RFCOMM module's pure-logic pieces.
//! Socket / kernel tests live behind `RUN_BT_HW_TESTS=1` in dedicated
//! follow-up files; this file runs on every platform.

use super::{format_bdaddr, node_id_to_hex, parse_bdaddr, BdAddr};

#[test]
fn bdaddr_roundtrip() {
    let s = "AA:BB:CC:DD:EE:FF";
    let parsed = parse_bdaddr(s).expect("parse");
    let back = format_bdaddr(&parsed);
    assert_eq!(back, s);
}

#[test]
fn bdaddr_parse_rejects_garbage() {
    assert!(parse_bdaddr("").is_none());
    assert!(parse_bdaddr("AA:BB").is_none());
    assert!(parse_bdaddr("XX:YY:ZZ:00:11:22").is_none());
}

#[test]
fn bdaddr_kernel_endianness() {
    // Wire string AA:BB:CC:DD:EE:FF maps to kernel bytes [FF,EE,DD,CC,BB,AA].
    let parsed = parse_bdaddr("AA:BB:CC:DD:EE:FF").unwrap();
    let expected: BdAddr = [0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA];
    assert_eq!(parsed, expected);
}

#[test]
fn node_id_to_hex_roundtrip() {
    let id = [0x01u8; 32];
    let hex = node_id_to_hex(&id);
    assert_eq!(hex.len(), 64);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(&hex[..6], "010101");
}

#[test]
fn iso_timestamp_format() {
    let s = super::now_iso();
    // Format YYYY-MM-DDTHH:MM:SSZ
    assert_eq!(s.len(), 20);
    assert!(s.ends_with("Z"));
    assert_eq!(&s[4..5], "-");
    assert_eq!(&s[10..11], "T");
}
