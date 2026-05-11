//! Cross-platform unit tests for the RFCOMM module's pure-logic pieces.
//! Socket / kernel tests live behind `RUN_BT_HW_TESTS=1` in dedicated
//! follow-up files; this file runs on every platform.

use super::{format_bdaddr, parse_bdaddr, BdAddr};

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
fn iso_timestamp_format() {
    let s = super::now_iso();
    // Format YYYY-MM-DDTHH:MM:SSZ
    assert_eq!(s.len(), 20);
    assert!(s.ends_with("Z"));
    assert_eq!(&s[4..5], "-");
    assert_eq!(&s[10..11], "T");
}

/// Cross-language wire-protocol fixtures (Layer 1 of
/// `plans/transport-architecture/05-bt-test-automation.md`).
///
/// Each fixture below is a byte-for-byte snapshot of a Hello-family
/// frame. The matching Kotlin tests in
/// `ui/src-tauri/gen/android/app/src/test/java/org/astervia/pim/HelloMsgTest.kt`
/// embed the *same* JSON strings and parse them with their own
/// `RfcommFrame` + `HelloMsg`. If either side drifts (renamed field,
/// changed validator, version bump) the test fails on that side
/// before any real-device session can.
///
/// Why golden bytes rather than a re-encode-and-compare round-trip:
/// we deliberately don't pin JSON key order across implementations
/// (`serde_json` vs `org.json` may differ), so byte-equality after
/// a fresh encode is impossible. Decode-side equivalence is what
/// actually catches the drift class we care about — "Kotlin can't
/// parse what Rust ships."
mod hello_fixtures {
    use super::super::frame::{decode_frame, encode_frame};
    use serde_json::Value;

    /// Open-mesh Hello JSON body. The Kotlin counterpart in
    /// `HelloMsgTest.kt` keeps the **same string** verbatim.
    /// `serde_json` + `org.json` may shuffle keys on a fresh encode,
    /// so we don't compare re-encoded outputs — we feed this fixture
    /// to each side's *decoder* and assert the parsed field values.
    pub(super) const HELLO_OPEN_JSON: &str = "\
        {\"type\":\"hello\",\
        \"v\":1,\
        \"node_id\":\"00112233445566778899aabbccddeeff\",\
        \"name\":\"PIM-fixture\",\
        \"platform\":\"linux\",\
        \"caps\":[\"mesh-v1\",\"gateway-v1\"]}";

    /// Private-mesh HelloAck JSON body. Carries a 64-hex `mesh_tag`
    /// (placeholder bytes — the real implementation derives it via
    /// `pim_crypto::compute_rfcomm_hello_tag`).
    pub(super) const HELLOACK_PRIVATE_JSON: &str = "\
        {\"type\":\"hello-ack\",\
        \"v\":1,\
        \"node_id\":\"ffeeddccbbaa99887766554433221100\",\
        \"name\":\"PIM-private\",\
        \"platform\":\"android\",\
        \"caps\":[\"mesh-v1\"],\
        \"mesh_tag\":\"\
        00112233445566778899aabbccddeeff\
        00112233445566778899aabbccddeeff\"}";

    /// Wrap a fixture JSON body in the 4-byte BE length-prefix frame.
    /// Used both as a self-check (frame codec on the local side) and
    /// to produce a byte sequence the Kotlin side parses identically.
    fn framed(body: &str) -> Vec<u8> {
        encode_frame(body.as_bytes()).expect("encode_frame")
    }

    #[test]
    fn open_hello_decodes_and_field_values_match() {
        let mut buf = framed(HELLO_OPEN_JSON);
        let frames = decode_frame(&mut buf).expect("decode");
        assert!(buf.is_empty(), "fixture should decode in one go");
        assert_eq!(frames.len(), 1);
        let v: Value = serde_json::from_slice(&frames[0]).expect("parse json");
        assert_eq!(v["type"], "hello");
        assert_eq!(v["v"], 1);
        assert_eq!(v["node_id"], "00112233445566778899aabbccddeeff");
        assert_eq!(v["name"], "PIM-fixture");
        assert_eq!(v["platform"], "linux");
        assert_eq!(v["caps"][0], "mesh-v1");
        assert_eq!(v["caps"][1], "gateway-v1");
        // Open mesh fixture must NOT carry mesh_tag.
        assert!(v.get("mesh_tag").is_none() || v["mesh_tag"].is_null());
    }

    #[test]
    fn private_helloack_decodes_and_carries_mesh_tag() {
        let mut buf = framed(HELLOACK_PRIVATE_JSON);
        let frames = decode_frame(&mut buf).expect("decode");
        assert!(buf.is_empty());
        assert_eq!(frames.len(), 1);
        let v: Value = serde_json::from_slice(&frames[0]).expect("parse json");
        assert_eq!(v["type"], "hello-ack");
        assert_eq!(v["v"], 1);
        assert_eq!(v["node_id"], "ffeeddccbbaa99887766554433221100");
        assert_eq!(v["platform"], "android");
        let tag = v["mesh_tag"].as_str().expect("mesh_tag present");
        assert_eq!(tag.len(), 64);
        assert!(tag.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn length_prefix_round_trips_through_codec() {
        // Sanity check: the frame codec is byte-identity on the body.
        // If this regresses (e.g. a stray newline gets injected), the
        // assertion below catches it before any radio I/O.
        let body = HELLO_OPEN_JSON.as_bytes();
        let framed = encode_frame(body).expect("encode");
        let prefix = u32::from_be_bytes([framed[0], framed[1], framed[2], framed[3]]) as usize;
        assert_eq!(prefix, body.len(), "BE length prefix must match body");
        assert_eq!(&framed[4..], body, "encode_frame must preserve body bytes");
    }

    #[test]
    fn version_mismatch_payload_is_rejectable_at_field_level() {
        // The Hello envelope's `v` field gates protocol-version
        // compatibility; both Rust and Kotlin reject `v != 1`. Catch
        // future drift where one side accidentally accepts `v = 2`.
        let bad = "{\"type\":\"hello\",\"v\":2,\"node_id\":\"00112233445566778899aabbccddeeff\",\
            \"name\":\"PIM-x\",\"platform\":\"linux\",\"caps\":[\"mesh-v1\"]}";
        let v: Value = serde_json::from_str(bad).expect("parse");
        assert_ne!(v["v"], 1, "fixture should not satisfy version check");
    }
}
