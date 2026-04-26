use super::super::*;

#[test]
fn parse_utun_unit_accepts_numbered_name() {
    assert_eq!(parse_utun_unit("utun0").unwrap(), 1);
    assert_eq!(parse_utun_unit("utun7").unwrap(), 8);
}

#[test]
fn parse_utun_unit_rejects_non_utun_name() {
    assert!(matches!(
        parse_utun_unit("pim0"),
        Err(TunError::UnsupportedInterfaceName(_))
    ));
}
