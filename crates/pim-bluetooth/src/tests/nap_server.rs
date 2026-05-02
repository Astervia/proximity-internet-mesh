use super::super::*;
use super::{make_executable, unique_test_dir};
use std::fs;

#[cfg(target_os = "linux")]
#[tokio::test]
async fn start_nap_server_auto_creates_bridge_and_invokes_bt_network_with_bridge() {
    let fake_root = unique_test_dir("pim-bt-fake-root-nap-auto");
    fs::create_dir_all(fake_root.join("sysfs")).unwrap();

    let fake_bt_network = fake_root.join("bt-network");
    let fake_args = fake_root.join("bt-network-args");
    fs::write(
        &fake_bt_network,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {args}\nexit 0\n",
            args = fake_args.display(),
        ),
    )
    .unwrap();
    let fake_bluetoothctl = fake_root.join("bluetoothctl");
    fs::write(&fake_bluetoothctl, "#!/bin/sh\nexit 0\n").unwrap();
    let fake_ip = fake_root.join("ip");
    let fake_ip_log = fake_root.join("ip-invocations");
    fs::write(
        &fake_ip,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nexit 0\n",
            log = fake_ip_log.display(),
        ),
    )
    .unwrap();
    let fake_iptables = fake_root.join("iptables");
    fs::write(&fake_iptables, "#!/bin/sh\nexit 0\n").unwrap();
    let fake_dnsmasq = fake_root.join("dnsmasq");
    fs::write(&fake_dnsmasq, "#!/bin/sh\nexit 0\n").unwrap();
    let fake_dhclient = fake_root.join("dhclient");
    fs::write(&fake_dhclient, "#!/bin/sh\nexit 0\n").unwrap();
    let fake_resolv = fake_root.join("resolv.conf");
    fs::write(&fake_resolv, "nameserver 1.1.1.1\n").unwrap();
    make_executable(&fake_bt_network);
    make_executable(&fake_bluetoothctl);
    make_executable(&fake_ip);
    make_executable(&fake_iptables);
    make_executable(&fake_dnsmasq);
    make_executable(&fake_dhclient);

    let config = BluetoothConfig {
        serve_nap: true,
        connect_pan: false,
        nap_bridge: "br-bt".into(),
        nap_bridge_addr: "192.168.44.1/24".into(),
        ..Default::default()
    };

    let (svc, _rx) = BluetoothDiscovery::new_with_system_paths(
        config,
        9100,
        Vec::new(),
        fake_root.join("sysfs"),
        fake_ip,
        fake_bluetoothctl,
        fake_bt_network,
        fake_iptables,
        fake_dnsmasq,
        fake_dhclient,
        fake_resolv,
        None::<String>,
    )
    .unwrap();

    let mut child = svc.start_nap_server().await.unwrap();
    child.wait().await.unwrap();

    let args = fs::read_to_string(&fake_args).unwrap();
    assert_eq!(args, "-s\nnap\nbr-bt\n");

    let ip_log = fs::read_to_string(&fake_ip_log).unwrap();
    assert!(
        ip_log.contains("link add name br-bt type bridge"),
        "expected bridge creation in ip log: {ip_log}"
    );
    assert!(
        ip_log.contains("link set br-bt up"),
        "expected bridge up in ip log: {ip_log}"
    );
    assert!(
        ip_log.contains("addr add 192.168.44.1/24 dev br-bt"),
        "expected address assignment in ip log: {ip_log}"
    );

    let _ = fs::remove_dir_all(fake_root);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn start_nap_server_rejects_empty_bridge() {
    let fake_root = unique_test_dir("pim-bt-fake-root-nap-empty");
    fs::create_dir_all(fake_root.join("sysfs")).unwrap();

    let fake_bt_network = fake_root.join("bt-network");
    fs::write(&fake_bt_network, "#!/bin/sh\nexit 0\n").unwrap();
    let fake_bluetoothctl = fake_root.join("bluetoothctl");
    fs::write(&fake_bluetoothctl, "#!/bin/sh\nexit 0\n").unwrap();
    let fake_ip = fake_root.join("ip");
    fs::write(&fake_ip, "#!/bin/sh\nexit 0\n").unwrap();
    let fake_iptables = fake_root.join("iptables");
    fs::write(&fake_iptables, "#!/bin/sh\nexit 0\n").unwrap();
    let fake_dnsmasq = fake_root.join("dnsmasq");
    fs::write(&fake_dnsmasq, "#!/bin/sh\nexit 0\n").unwrap();
    let fake_dhclient = fake_root.join("dhclient");
    fs::write(&fake_dhclient, "#!/bin/sh\nexit 0\n").unwrap();
    let fake_resolv = fake_root.join("resolv.conf");
    fs::write(&fake_resolv, "nameserver 1.1.1.1\n").unwrap();
    make_executable(&fake_bt_network);
    make_executable(&fake_bluetoothctl);
    make_executable(&fake_ip);
    make_executable(&fake_iptables);
    make_executable(&fake_dnsmasq);
    make_executable(&fake_dhclient);

    let config = BluetoothConfig {
        serve_nap: true,
        connect_pan: false,
        nap_bridge: "".into(),
        ..Default::default()
    };

    let (svc, _rx) = BluetoothDiscovery::new_with_system_paths(
        config,
        9100,
        Vec::new(),
        fake_root.join("sysfs"),
        fake_ip,
        fake_bluetoothctl,
        fake_bt_network,
        fake_iptables,
        fake_dnsmasq,
        fake_dhclient,
        fake_resolv,
        None::<String>,
    )
    .unwrap();

    let err = svc.start_nap_server().await.unwrap_err();
    match err {
        BluetoothError::CommandFailed { command, message } => {
            assert_eq!(command, "bt-network");
            assert!(message.contains("non-empty nap_bridge"), "got: {message}");
        }
        other => panic!("expected CommandFailed, got {other:?}"),
    }

    let _ = fs::remove_dir_all(fake_root);
}
