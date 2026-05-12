//! Cross-platform unit tests for the CoC module's pure-logic pieces.
//! Socket / kernel tests live behind `RUN_BT_HW_TESTS=1` on Linux;
//! everything here runs on every platform.

use super::{format_bdaddr, parse_bdaddr, BdAddr, DEFAULT_PSM};

#[test]
fn bdaddr_roundtrip() {
    let s = "AA:BB:CC:DD:EE:FF";
    let parsed = parse_bdaddr(s).expect("parse");
    let back = format_bdaddr(&parsed);
    assert_eq!(back, s);
}

#[test]
fn bdaddr_kernel_endianness() {
    let parsed = parse_bdaddr("AA:BB:CC:DD:EE:FF").unwrap();
    let expected: BdAddr = [0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA];
    assert_eq!(parsed, expected);
}

#[test]
fn default_psm_is_in_le_dynamic_range() {
    // LE-only dynamic PSM range is 0x0080..=0x00FF; values in
    // 0x0001..=0x007F are SIG-assigned and forbidden for application
    // use. Lock the default so a misconfigured override doesn't
    // silently pick a reserved value.
    assert!(
        (0x0080..=0x00FF).contains(&DEFAULT_PSM),
        "DEFAULT_PSM {DEFAULT_PSM:#06x} is not in the LE dynamic range 0x0080..=0x00FF"
    );
}
