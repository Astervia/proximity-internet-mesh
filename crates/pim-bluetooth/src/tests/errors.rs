use super::super::*;

#[test]
fn missing_device_ip_error_is_classified_as_transient() {
    let err = BluetoothError::CommandFailed {
        command: "ip",
        message: "Cannot find device \"enx6432a8144f4b\"".into(),
    };
    assert!(err.is_missing_device_error());

    let other = BluetoothError::CommandFailed {
        command: "ip",
        message: "RTNETLINK answers: File exists".into(),
    };
    assert!(!other.is_missing_device_error());
}
