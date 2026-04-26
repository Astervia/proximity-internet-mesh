use super::super::*;

#[test]
fn error_display() {
    let err = PimError::Crypto("bad key".into());
    assert_eq!(format!("{err}"), "crypto error: bad key");
}

#[test]
fn error_from_io() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
    let pim_err: PimError = io_err.into();
    assert!(format!("{pim_err}").contains("gone"));
}
