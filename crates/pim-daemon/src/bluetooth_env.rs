use std::path::PathBuf;

pub(crate) fn bluetooth_sysfs_root_from_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pim_bluetooth::DEFAULT_SYSFS_ROOT))
}

pub(crate) fn bluetooth_sysfs_root() -> PathBuf {
    bluetooth_sysfs_root_from_env(std::env::var_os("PIM_BLUETOOTH_SYSFS_ROOT"))
}

pub(crate) fn bluetooth_ip_command_from_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pim_bluetooth::DEFAULT_IP_COMMAND))
}

pub(crate) fn bluetooth_ip_command() -> PathBuf {
    bluetooth_ip_command_from_env(std::env::var_os("PIM_BLUETOOTH_IP_COMMAND"))
}

pub(crate) fn bluetoothctl_command_from_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pim_bluetooth::DEFAULT_BLUETOOTHCTL_COMMAND))
}

pub(crate) fn bluetoothctl_command() -> PathBuf {
    bluetoothctl_command_from_env(std::env::var_os("PIM_BLUETOOTH_BLUETOOTHCTL_COMMAND"))
}

pub(crate) fn bt_network_command_from_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pim_bluetooth::DEFAULT_BT_NETWORK_COMMAND))
}

pub(crate) fn bt_network_command() -> PathBuf {
    bt_network_command_from_env(std::env::var_os("PIM_BLUETOOTH_BT_NETWORK_COMMAND"))
}

pub(crate) fn bluetooth_iptables_command_from_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pim_bluetooth::DEFAULT_IPTABLES_COMMAND))
}

pub(crate) fn bluetooth_iptables_command() -> PathBuf {
    bluetooth_iptables_command_from_env(std::env::var_os("PIM_BLUETOOTH_IPTABLES_COMMAND"))
}

pub(crate) fn bluetooth_dnsmasq_command_from_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pim_bluetooth::DEFAULT_DNSMASQ_COMMAND))
}

pub(crate) fn bluetooth_dnsmasq_command() -> PathBuf {
    bluetooth_dnsmasq_command_from_env(std::env::var_os("PIM_BLUETOOTH_DNSMASQ_COMMAND"))
}

pub(crate) fn bluetooth_dhclient_command_from_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pim_bluetooth::DEFAULT_DHCLIENT_COMMAND))
}

pub(crate) fn bluetooth_dhclient_command() -> PathBuf {
    bluetooth_dhclient_command_from_env(std::env::var_os("PIM_BLUETOOTH_DHCLIENT_COMMAND"))
}

pub(crate) fn bluetooth_resolv_conf_path_from_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pim_bluetooth::DEFAULT_RESOLV_CONF))
}

pub(crate) fn bluetooth_resolv_conf_path() -> PathBuf {
    bluetooth_resolv_conf_path_from_env(std::env::var_os("PIM_BLUETOOTH_RESOLV_CONF"))
}

#[cfg(any(test, target_os = "macos"))]
pub(crate) fn macos_bluetooth_config_warnings(config: &pim_core::BluetoothConfig) -> Vec<String> {
    let mut warnings = Vec::new();

    if config.serve_nap {
        warnings.push(
            "macOS ignores [bluetooth].serve_nap; daemon-managed NAP server mode is Linux-only"
                .to_string(),
        );
    }

    if !config.nap_bridge.trim().is_empty() && config.nap_bridge != "bridge0" {
        warnings.push(format!(
            "macOS ignores [bluetooth].nap_bridge = {:?}; NAP bridge management is Linux-only",
            config.nap_bridge
        ));
    }

    warnings
}
