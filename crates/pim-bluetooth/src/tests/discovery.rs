use super::super::*;
use super::{make_executable, unique_test_dir};
use std::fs;

#[test]
fn discovery_new_returns_receiver_and_targets() {
    let config = BluetoothConfig {
        radio_discovery_enabled: false,
        auto_discover_peers: false,
        ..Default::default()
    };

    let (svc, _rx) = BluetoothDiscovery::new(
        config,
        9100,
        vec![
            "192.168.44.2:9100".parse().unwrap(),
            "[fd00::2]:9100".parse().unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(svc.target_socket_addrs().len(), 2);
    assert_eq!(
        svc.target_socket_addrs()[0],
        "192.168.44.2:9100".parse().unwrap()
    );
    assert_eq!(
        svc.target_socket_addrs()[1],
        "[fd00::2]:9100".parse().unwrap()
    );
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn run_discovers_radio_peer_and_emits_neighbor_target() {
    let fake_root = unique_test_dir("pim-bt-fake-root");
    fs::create_dir_all(fake_root.join("sysfs/bnep0")).unwrap();
    fs::write(fake_root.join("sysfs/bnep0/operstate"), "down\n").unwrap();

    let fake_bluetoothctl = fake_root.join("bluetoothctl");
    fs::write(
        &fake_bluetoothctl,
        "#!/bin/sh\nif [ \"$3\" = \"devices\" ]; then\n  printf 'Device AA:BB:CC:DD:EE:FF PIM-peer\\n'\n  exit 0\nfi\nexit 0\n",
    )
    .unwrap();
    let fake_bt_network = fake_root.join("bt-network");
    fs::write(
        &fake_bt_network,
        format!(
            "#!/bin/sh\ntouch {ready}\nprintf 'up\\n' > {operstate}\nexit 0\n",
            ready = fake_root.join("fake-neigh-ready").display(),
            operstate = fake_root.join("sysfs/bnep0/operstate").display(),
        ),
    )
    .unwrap();
    let fake_ip = fake_root.join("ip");
    fs::write(
        &fake_ip,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"neigh\" ] && [ \"$2\" = \"show\" ] && [ \"$3\" = \"dev\" ] && [ \"$4\" = \"bnep0\" ]; then\n  if [ -f {ready} ]; then\n    printf '192.168.44.2 dev bnep0 lladdr 02:00:00:00:00:02 REACHABLE\\n'\n  fi\n  exit 0\nfi\nexit 0\n",
            ready = fake_root.join("fake-neigh-ready").display(),
        ),
    )
    .unwrap();
    make_executable(&fake_bluetoothctl);
    make_executable(&fake_bt_network);
    make_executable(&fake_ip);

    let config = BluetoothConfig {
        interface: "bnep0".into(),
        local_alias: "PIM-self".into(),
        poll_interval_ms: 10,
        scan_interval_ms: 10,
        peer_discovery_interval_ms: 10,
        startup_timeout_ms: 500,
        bluetoothctl_timeout_s: 1,
        request_dhcp: false,
        ..Default::default()
    };

    let fake_iptables = fake_root.join("iptables");
    fs::write(&fake_iptables, "#!/bin/sh\nexit 0\n").unwrap();
    make_executable(&fake_iptables);
    let fake_dnsmasq = fake_root.join("dnsmasq");
    fs::write(&fake_dnsmasq, "#!/bin/sh\nexit 0\n").unwrap();
    make_executable(&fake_dnsmasq);
    let fake_dhclient = fake_root.join("dhclient");
    fs::write(&fake_dhclient, "#!/bin/sh\nexit 0\n").unwrap();
    make_executable(&fake_dhclient);
    let fake_resolv = fake_root.join("resolv.conf");
    fs::write(&fake_resolv, "nameserver 1.1.1.1\n").unwrap();

    let (svc, mut rx) = BluetoothDiscovery::new_with_system_paths(
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
    let cancel = CancellationToken::new();

    let runner = tokio::spawn({
        let cancel = cancel.clone();
        async move { svc.run(cancel).await }
    });

    let addr = tokio::time::timeout(Duration::from_millis(300), rx.recv())
        .await
        .expect("timed out waiting for Bluetooth address")
        .expect("channel closed before address emitted");
    assert_eq!(addr, "192.168.44.2:9100".parse().unwrap());

    cancel.cancel();
    runner.await.unwrap().unwrap();
    let _ = fs::remove_dir_all(fake_root);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn run_resolves_dynamic_enx_interface() {
    let fake_root = unique_test_dir("pim-bt-fake-root-enx");
    fs::create_dir_all(fake_root.join("sysfs/enx6432a8144f4b")).unwrap();
    fs::write(fake_root.join("sysfs/enx6432a8144f4b/operstate"), "up\n").unwrap();

    let fake_bluetoothctl = fake_root.join("bluetoothctl");
    fs::write(&fake_bluetoothctl, "#!/bin/sh\nexit 0\n").unwrap();

    let fake_bt_network = fake_root.join("bt-network");
    fs::write(&fake_bt_network, "#!/bin/sh\nexit 0\n").unwrap();

    let fake_ip = fake_root.join("ip");
    fs::write(
        &fake_ip,
        "#!/bin/sh\nif [ \"$1\" = \"neigh\" ] && [ \"$2\" = \"show\" ] && [ \"$3\" = \"dev\" ] && [ \"$4\" = \"enx6432a8144f4b\" ]; then\n  printf '192.168.44.9 dev enx6432a8144f4b lladdr 02:00:00:00:00:09 REACHABLE\\n'\n  exit 0\nfi\nexit 0\n",
    )
    .unwrap();
    make_executable(&fake_bluetoothctl);
    make_executable(&fake_bt_network);
    make_executable(&fake_ip);

    let config = BluetoothConfig {
        interface: "bnep0".into(),
        radio_discovery_enabled: false,
        local_alias: "PIM-self".into(),
        poll_interval_ms: 10,
        peer_discovery_interval_ms: 10,
        startup_timeout_ms: 500,
        request_dhcp: false,
        ..Default::default()
    };

    let fake_iptables = fake_root.join("iptables");
    fs::write(&fake_iptables, "#!/bin/sh\nexit 0\n").unwrap();
    make_executable(&fake_iptables);
    let fake_dnsmasq = fake_root.join("dnsmasq");
    fs::write(&fake_dnsmasq, "#!/bin/sh\nexit 0\n").unwrap();
    make_executable(&fake_dnsmasq);
    let fake_dhclient = fake_root.join("dhclient");
    fs::write(&fake_dhclient, "#!/bin/sh\nexit 0\n").unwrap();
    make_executable(&fake_dhclient);
    let fake_resolv = fake_root.join("resolv.conf");
    fs::write(&fake_resolv, "nameserver 1.1.1.1\n").unwrap();

    let (svc, mut rx) = BluetoothDiscovery::new_with_system_paths(
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
    let cancel = CancellationToken::new();

    let runner = tokio::spawn({
        let cancel = cancel.clone();
        async move { svc.run(cancel).await }
    });

    let addr = tokio::time::timeout(Duration::from_millis(300), rx.recv())
        .await
        .expect("timed out waiting for dynamic enx Bluetooth address")
        .expect("channel closed before address emitted");
    assert_eq!(addr, "192.168.44.9:9100".parse().unwrap());

    cancel.cancel();
    runner.await.unwrap().unwrap();
    let _ = fs::remove_dir_all(fake_root);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn run_discovers_neighbors_across_multiple_pan_interfaces() {
    let fake_root = unique_test_dir("pim-bt-fake-root-multi-pan");
    fs::create_dir_all(fake_root.join("sysfs/bnep0")).unwrap();
    fs::create_dir_all(fake_root.join("sysfs/enx6432a8144f4b")).unwrap();
    fs::write(fake_root.join("sysfs/bnep0/operstate"), "up\n").unwrap();
    fs::write(fake_root.join("sysfs/enx6432a8144f4b/operstate"), "up\n").unwrap();

    let fake_bluetoothctl = fake_root.join("bluetoothctl");
    fs::write(&fake_bluetoothctl, "#!/bin/sh\nexit 0\n").unwrap();

    let fake_bt_network = fake_root.join("bt-network");
    fs::write(&fake_bt_network, "#!/bin/sh\nexit 0\n").unwrap();

    let fake_ip = fake_root.join("ip");
    fs::write(
        &fake_ip,
        "#!/bin/sh\nif [ \"$1\" = \"neigh\" ] && [ \"$2\" = \"show\" ] && [ \"$3\" = \"dev\" ] && [ \"$4\" = \"bnep0\" ]; then\n  printf '192.168.44.2 dev bnep0 lladdr 02:00:00:00:00:02 REACHABLE\\n'\n  exit 0\nfi\nif [ \"$1\" = \"neigh\" ] && [ \"$2\" = \"show\" ] && [ \"$3\" = \"dev\" ] && [ \"$4\" = \"enx6432a8144f4b\" ]; then\n  printf '192.168.44.9 dev enx6432a8144f4b lladdr 02:00:00:00:00:09 REACHABLE\\n'\n  exit 0\nfi\nexit 0\n",
    )
    .unwrap();
    make_executable(&fake_bluetoothctl);
    make_executable(&fake_bt_network);
    make_executable(&fake_ip);

    let config = BluetoothConfig {
        interface: "auto".into(),
        radio_discovery_enabled: false,
        local_alias: "PIM-self".into(),
        poll_interval_ms: 10,
        peer_discovery_interval_ms: 10,
        startup_timeout_ms: 500,
        request_dhcp: false,
        ..Default::default()
    };

    let fake_iptables = fake_root.join("iptables");
    fs::write(&fake_iptables, "#!/bin/sh\nexit 0\n").unwrap();
    make_executable(&fake_iptables);
    let fake_dnsmasq = fake_root.join("dnsmasq");
    fs::write(&fake_dnsmasq, "#!/bin/sh\nexit 0\n").unwrap();
    make_executable(&fake_dnsmasq);
    let fake_dhclient = fake_root.join("dhclient");
    fs::write(&fake_dhclient, "#!/bin/sh\nexit 0\n").unwrap();
    make_executable(&fake_dhclient);
    let fake_resolv = fake_root.join("resolv.conf");
    fs::write(&fake_resolv, "nameserver 1.1.1.1\n").unwrap();

    let (svc, mut rx) = BluetoothDiscovery::new_with_system_paths(
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
    let cancel = CancellationToken::new();

    let runner = tokio::spawn({
        let cancel = cancel.clone();
        async move { svc.run(cancel).await }
    });

    let addr_a = tokio::time::timeout(Duration::from_millis(300), rx.recv())
        .await
        .expect("timed out waiting for first multi-interface Bluetooth address")
        .expect("channel closed before first address emitted");
    let addr_b = tokio::time::timeout(Duration::from_millis(300), rx.recv())
        .await
        .expect("timed out waiting for second multi-interface Bluetooth address")
        .expect("channel closed before second address emitted");

    let received: HashSet<SocketAddr> = [addr_a, addr_b].into_iter().collect();
    assert!(received.contains(&"192.168.44.2:9100".parse().unwrap()));
    assert!(received.contains(&"192.168.44.9:9100".parse().unwrap()));

    cancel.cancel();
    runner.await.unwrap().unwrap();
    fs::remove_dir_all(fake_root).unwrap();
}
