use super::*;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

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

#[test]
fn operstate_helper_accepts_up_and_unknown() {
    assert!(is_ready_operstate("up\n"));
    assert!(is_ready_operstate("unknown"));
    assert!(!is_ready_operstate("down"));
}

#[test]
fn ifconfig_output_treats_non_inactive_interface_as_ready() {
    let active = "\
bridge0: flags=41<UP,RUNNING> mtu 1500\n\
\tstatus: active\n";
    let no_status = "bridge0: flags=41<UP,RUNNING> mtu 1500\n";
    let inactive = "\
bridge0: flags=41<UP,RUNNING> mtu 1500\n\
\tstatus: inactive\n";
    assert!(is_ready_ifconfig_output(active));
    assert!(is_ready_ifconfig_output(no_status));
    assert!(!is_ready_ifconfig_output(inactive));
}

#[test]
fn macos_auto_interface_hint_defaults_to_bridge0() {
    assert_eq!(resolve_macos_pan_interface_hint("auto"), "bridge0");
    assert_eq!(resolve_macos_pan_interface_hint(""), "bridge0");
    assert_eq!(resolve_macos_pan_interface_hint("bridge1"), "bridge1");
}

#[test]
fn neighbor_output_parses_ipv4_and_ipv6_and_skips_failed_entries() {
    let output = "\
192.168.44.2 dev bnep0 lladdr 02:00:00:00:00:02 REACHABLE
fe80::1234 dev bnep0 lladdr 02:00:00:00:00:03 router STALE
192.168.44.3 dev bnep0 FAILED
";
    let parsed = parse_neighbor_output(output, 9100, Some(7));
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0], "192.168.44.2:9100".parse().unwrap());
    assert_eq!(
        parsed[1],
        SocketAddr::V6(SocketAddrV6::new("fe80::1234".parse().unwrap(), 9100, 0, 7))
    );
}

#[test]
fn devices_output_filters_to_matching_prefix() {
    let output = "\
Device AA:BB:CC:DD:EE:01 PIM-gateway
Device AA:BB:CC:DD:EE:02 Phone
Device AA:BB:CC:DD:EE:03 PIM-client
";
    let parsed = parse_devices_output(output, "PIM-", "PIM-self");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].mac, "AA:BB:CC:DD:EE:01");
    assert_eq!(parsed[1].name, "PIM-client");
}

#[test]
fn blueutil_inquiry_output_filters_to_matching_prefix() {
    let output = "\
address: aa-bb-cc-dd-ee-01, not connected, not favourite, not paired, name: \"PIM-gateway\", recent access date: -\n\
address: aa-bb-cc-dd-ee-02, not connected, not favourite, not paired, name: \"Phone\", recent access date: -\n\
address: aa-bb-cc-dd-ee-03, not connected, not favourite, not paired, name: \"PIM-self\", recent access date: -\n";
    let parsed = parse_blueutil_inquiry_output(output, "PIM-", "PIM-self");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].mac, "AA:BB:CC:DD:EE:01");
    assert_eq!(parsed[0].name, "PIM-gateway");
}

#[test]
fn arp_output_parses_interface_scoped_neighbors() {
    let output = "\
? (192.168.44.2) at aa:bb:cc:dd:ee:01 on bridge0 ifscope [ethernet]\n\
? (192.168.44.3) at aa:bb:cc:dd:ee:02 on en0 ifscope [ethernet]\n\
? (fe80::1) at aa:bb:cc:dd:ee:03 on bridge0 ifscope permanent [ethernet]\n";
    let parsed = parse_arp_output(output, "bridge0", 9100);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0], "192.168.44.2:9100".parse().unwrap());
    assert_eq!(parsed[1], "[fe80::1]:9100".parse().unwrap());
}

#[test]
fn interface_operstate_path_uses_supplied_root() {
    let path = interface_operstate_path(Path::new("/tmp/fake-sysfs"), "bnep0");
    assert_eq!(path, PathBuf::from("/tmp/fake-sysfs/bnep0/operstate"));
}

#[test]
fn preferred_interface_hint_treats_auto_as_unset() {
    assert_eq!(preferred_interface_hint("auto"), None);
    assert_eq!(preferred_interface_hint(""), None);
    assert_eq!(preferred_interface_hint("bnep0"), Some("bnep0"));
}

#[test]
fn select_pan_interfaces_prefers_configured_ready_interface() {
    let selected = select_pan_interfaces(
        &[
            PanInterfaceCandidate {
                name: "enx1234".into(),
                operstate: Some("up".into()),
            },
            PanInterfaceCandidate {
                name: "bnep7".into(),
                operstate: Some("up".into()),
            },
        ],
        Some("enx1234"),
        None,
    );
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "enx1234");
    assert_eq!(selected[0].source, "configured");
}

#[test]
fn select_pan_interfaces_fall_back_to_dynamic_linux_names() {
    let selected = select_pan_interfaces(
        &[
            PanInterfaceCandidate {
                name: "eth0".into(),
                operstate: Some("up".into()),
            },
            PanInterfaceCandidate {
                name: "enx6432a8144f4b".into(),
                operstate: Some("up".into()),
            },
        ],
        Some("bnep0"),
        None,
    );
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "enx6432a8144f4b");
    assert_eq!(selected[0].source, "dynamic-enx");
}

#[test]
fn select_pan_interfaces_use_nap_bridge_when_serving() {
    let selected = select_pan_interfaces(
        &[PanInterfaceCandidate {
            name: "br-bt".into(),
            operstate: Some("down".into()),
        }],
        None,
        Some("br-bt"),
    );
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "br-bt");
    assert_eq!(selected[0].source, "nap_bridge");
}

#[test]
fn select_pan_interfaces_include_all_ready_dynamic_pan_links() {
    let selected = select_pan_interfaces(
        &[
            PanInterfaceCandidate {
                name: "bnep0".into(),
                operstate: Some("up".into()),
            },
            PanInterfaceCandidate {
                name: "enx6432a8144f4b".into(),
                operstate: Some("up".into()),
            },
            PanInterfaceCandidate {
                name: "eth0".into(),
                operstate: Some("up".into()),
            },
        ],
        None,
        None,
    );
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].name, "bnep0");
    assert_eq!(selected[1].name, "enx6432a8144f4b");
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

    let args = fs::read_to_string(fake_args).unwrap();
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

#[test]
fn parse_ipv4_cidr_accepts_common_cases() {
    let (ip, prefix) = parse_ipv4_cidr("192.168.44.1/24").unwrap();
    assert_eq!(ip, std::net::Ipv4Addr::new(192, 168, 44, 1));
    assert_eq!(prefix, 24);
    assert!(parse_ipv4_cidr("not a cidr").is_err());
    assert!(parse_ipv4_cidr("192.168.44.1/33").is_err());
}

#[test]
fn default_dhcp_range_keeps_gateway_out_of_pool() {
    let range = default_dhcp_range(std::net::Ipv4Addr::new(192, 168, 44, 1), 24).unwrap();
    assert_eq!(range, "192.168.44.10,192.168.44.245");
}

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

fn unique_test_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }
}
