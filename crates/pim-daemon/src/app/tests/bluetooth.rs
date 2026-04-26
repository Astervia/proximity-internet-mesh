use super::super::*;
use super::peer_id;

#[test]
fn bluetooth_disabled_config_skips_spawning() {
    let config = Config::from_toml_str("[node]\nname=\"t\"\n").unwrap();
    assert!(
        !config.bluetooth.enabled,
        "Bluetooth PAN watcher must not start when enabled=false"
    );
}

#[test]
fn bluetooth_enabled_config_exposes_interface_and_static_peer_entries() {
    let toml = "[node]\nname=\"t\"\n[bluetooth]\nenabled=true\ninterface=\"bnep1\"\n[[peers]]\nmechanism=\"bluetooth\"\nip=\"192.168.44.2\"\n";
    let config = Config::from_toml_str(toml).unwrap();
    assert!(config.bluetooth.enabled);
    assert_eq!(config.bluetooth.interface, "bnep1");
    assert!(config.bluetooth.connect_pan);
    assert!(!config.bluetooth.serve_nap);
    assert_eq!(config.peers.len(), 1);
    assert_eq!(config.transport.listen_port, 9100);
}

#[tokio::test]
async fn bluetooth_addr_registered_for_reconnect() {
    let mgr = ReconnectManager::new([]);
    let addr: SocketAddr = "192.168.44.2:9100".parse().unwrap();
    let target = ConnectTarget::BluetoothPan(addr);
    mgr.register_discovered(target).await;
    mgr.register(peer_id(63), target).await;
    assert_eq!(
        mgr.is_reconnectable_target(&peer_id(63)).await,
        Some(target),
        "Bluetooth-discovered peer must be reconnectable"
    );
}

#[test]
fn bluetooth_discovery_construction_from_config() {
    use pim_bluetooth::BluetoothDiscovery;
    let toml = "[node]\nname=\"t\"\n[bluetooth]\nenabled=true\n";
    let config = Config::from_toml_str(toml).unwrap();
    let (_svc, _rx) = BluetoothDiscovery::new(
        config.bluetooth,
        config.transport.listen_port,
        vec!["192.168.44.2:9100".parse().unwrap()],
    )
    .unwrap();
}

#[test]
fn bluetooth_sysfs_root_defaults_to_platform_default() {
    assert_eq!(
        bluetooth_sysfs_root_from_env(None),
        PathBuf::from(pim_bluetooth::DEFAULT_SYSFS_ROOT)
    );
}

#[test]
fn bluetooth_sysfs_root_honors_environment_override() {
    assert_eq!(
        bluetooth_sysfs_root_from_env(Some("/tmp/pim-fake-sysfs".into())),
        PathBuf::from("/tmp/pim-fake-sysfs")
    );
}

#[test]
fn bluetooth_ip_command_defaults_to_platform_default() {
    assert_eq!(
        bluetooth_ip_command_from_env(None),
        PathBuf::from(pim_bluetooth::DEFAULT_IP_COMMAND)
    );
}

#[test]
fn bluetooth_ip_command_honors_environment_override() {
    assert_eq!(
        bluetooth_ip_command_from_env(Some("/tmp/fake-ip".into())),
        PathBuf::from("/tmp/fake-ip")
    );
}

#[test]
fn bluetoothctl_command_defaults_to_platform_default() {
    assert_eq!(
        bluetoothctl_command_from_env(None),
        PathBuf::from(pim_bluetooth::DEFAULT_BLUETOOTHCTL_COMMAND)
    );
}

#[test]
fn bluetoothctl_command_honors_environment_override() {
    assert_eq!(
        bluetoothctl_command_from_env(Some("/tmp/fake-bluetoothctl".into())),
        PathBuf::from("/tmp/fake-bluetoothctl")
    );
}

#[test]
fn bt_network_command_defaults_to_bt_network() {
    assert_eq!(
        bt_network_command_from_env(None),
        PathBuf::from("bt-network")
    );
}

#[test]
fn bt_network_command_honors_environment_override() {
    assert_eq!(
        bt_network_command_from_env(Some("/tmp/fake-bt-network".into())),
        PathBuf::from("/tmp/fake-bt-network")
    );
}

#[test]
fn macos_bluetooth_config_warning_omits_linux_only_fields_when_unused() {
    let warnings = macos_bluetooth_config_warnings(
        &Config::from_toml_str(
            "[node]\nname=\"t\"\n[bluetooth]\nenabled=true\nnap_bridge=\"bridge0\"\n",
        )
        .unwrap()
        .bluetooth,
    );
    assert!(warnings.is_empty());
}

#[test]
fn macos_bluetooth_config_warning_reports_linux_only_fields() {
    let warnings = macos_bluetooth_config_warnings(
        &Config::from_toml_str(
            "[node]\nname=\"t\"\n[bluetooth]\nenabled=true\nserve_nap=true\nnap_bridge=\"br-bt\"\n",
        )
        .unwrap()
        .bluetooth,
    );
    assert_eq!(warnings.len(), 2);
    assert!(warnings[0].contains("serve_nap"));
    assert!(warnings[1].contains("nap_bridge"));
}
