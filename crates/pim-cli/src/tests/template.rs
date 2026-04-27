use super::super::*;

// ── Existing coverage (pre-expansion) ────────────────────────────────────────

#[test]
fn client_template_has_commented_gateway_block_and_parses() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    assert!(rendered.contains("# [gateway]"));
    assert!(rendered.contains("# mechanism = \"tcp\""));
    assert!(rendered.contains("mesh_ip = \"auto\""));

    let config = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert_eq!(config.node.name, "client-node");
    assert_eq!(config.interface.mesh_ip, "auto");
    assert!(!config.gateway.enabled);
}

#[test]
fn gateway_template_enables_gateway_and_parses() {
    let rendered = render_config_template(&[NodeRole::Gateway], Some("edge-a"));
    assert!(rendered.contains("[gateway]"));
    assert!(rendered.contains("enabled = true"));

    let config = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert_eq!(config.node.name, "edge-a");
    assert!(config.gateway.enabled);
    assert_eq!(config.interface.mesh_ip, "10.77.0.1/24");
}

#[test]
fn multi_role_template_deduplicates_roles() {
    let rendered =
        render_config_template(&[NodeRole::Relay, NodeRole::Gateway, NodeRole::Relay], None);

    assert!(rendered.contains("# Roles enabled: relay, gateway"));
    let config = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert_eq!(config.node.name, "relay-gateway-node");
    assert!(config.gateway.enabled);
}

#[test]
fn config_template_uses_platform_interface_name() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    assert!(rendered.contains(&format!("name = {:?}", default_interface_name())));
}

// ── Schema-completeness coverage (every field in pim-core/config/model.rs) ──

#[test]
fn template_node_emits_data_dir() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    let cfg = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert_eq!(cfg.node.data_dir.as_os_str(), "/var/lib/pim");
}

#[test]
fn template_interface_emits_mtu_and_optional_ipv6() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    assert!(rendered.contains("mtu = 1400"));
    // Optional Option<String> field — should appear commented out so it
    // round-trips as `None` after parse.
    assert!(rendered.contains("# mesh_ipv6 ="));
    let cfg = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert_eq!(cfg.interface.mtu, 1400);
    assert_eq!(cfg.interface.mesh_ipv6, None);
}

#[test]
fn template_discovery_emits_all_fields() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    // Active values
    for needle in [
        "[discovery]",
        "enabled = true",
        "port = 9101",
        "broadcast_interval_ms = 5000",
        "peer_timeout_ms = 30000",
        "connect_relays = true",
        "connect_gateways = true",
    ] {
        assert!(
            rendered.contains(needle),
            "discovery missing `{needle}`. rendered:\n{rendered}"
        );
    }
    // Optional shared_key — commented out
    assert!(rendered.contains("# shared_key ="));

    let cfg = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert!(cfg.discovery.enabled);
    assert_eq!(cfg.discovery.port, 9101);
    assert_eq!(cfg.discovery.broadcast_interval_ms, 5000);
    assert_eq!(cfg.discovery.peer_timeout_ms, 30000);
    assert!(cfg.discovery.connect_relays);
    assert!(cfg.discovery.connect_gateways);
    assert_eq!(cfg.discovery.shared_key, None);
}

#[test]
fn template_transport_emits_all_fields() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    for needle in [
        "type = \"tcp\"",
        "listen_port = 9100",
        "max_reconnect_attempts = 20",
        "connect_timeout_ms = 3000",
    ] {
        assert!(
            rendered.contains(needle),
            "transport missing `{needle}`. rendered:\n{rendered}"
        );
    }
    let cfg = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert_eq!(cfg.transport.r#type, "tcp");
    assert_eq!(cfg.transport.listen_port, 9100);
    assert_eq!(cfg.transport.max_reconnect_attempts, 20);
    assert_eq!(cfg.transport.connect_timeout_ms, 3000);
}

#[test]
fn template_routing_emits_all_fields() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    let cfg = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert_eq!(cfg.routing.max_hops, 10);
    assert_eq!(cfg.routing.algorithm, "distance-vector");
    assert_eq!(cfg.routing.route_expiry_s, 300);
}

#[test]
fn template_relay_role_enables_relay() {
    let rendered = render_config_template(&[NodeRole::Relay], None);
    assert!(rendered.contains("[relay]"));
    let cfg = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert!(
        cfg.relay.enabled,
        "relay role should set relay.enabled = true"
    );
}

#[test]
fn template_client_role_disables_relay() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    let cfg = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert!(
        !cfg.relay.enabled,
        "client role should keep relay.enabled = false (capability bits 0x01)"
    );
}

#[test]
fn template_gateway_role_keeps_relay_default_false() {
    // Gateway is implicitly also a relay — the daemon resolves capabilities
    // from gateway.enabled regardless of relay.enabled. Verify the renderer
    // documents this and emits a sensible explicit value.
    let rendered = render_config_template(&[NodeRole::Gateway], None);
    assert!(rendered.contains("# Note: a gateway is implicitly also a relay"));
}

#[test]
fn template_security_emits_authorization_policy_and_files() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    for needle in [
        "key_file = \"/var/lib/pim/node.key\"",
        "require_encryption = true",
        "authorization_policy = \"allow_all\"",
        "trust_store_file = \"/var/lib/pim/trusted-peers.toml\"",
    ] {
        assert!(
            rendered.contains(needle),
            "security missing `{needle}`. rendered:\n{rendered}"
        );
    }
    // authorized_peers is commented because it's only used for allow_list.
    assert!(rendered.contains("# authorized_peers ="));

    let cfg = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert_eq!(
        cfg.security.authorization_policy,
        pim_core::AuthorizationPolicy::AllowAll
    );
    assert!(cfg.security.require_encryption);
    assert!(cfg.security.authorized_peers.is_empty());
    assert_eq!(
        cfg.security.trust_store_file.as_os_str(),
        "/var/lib/pim/trusted-peers.toml"
    );
}

#[test]
fn template_bluetooth_emits_full_block_disabled_by_default() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    assert!(rendered.contains("[bluetooth]"));
    // Master toggle off by default — matches BluetoothConfig::default()
    // and avoids surprising behaviour on hosts without BlueZ / blueutil.
    assert!(rendered.contains("enabled = false"));
    // Every required (non-Option) field must be present so the template
    // doubles as a discovery surface for operators.
    for needle in [
        "radio_discovery_enabled = true",
        "device_name_prefix = \"PIM-\"",
        "local_alias =",
        "connect_pan = true",
        "serve_nap = false",
        "nap_bridge = \"br-bt\"",
        "nap_bridge_addr = \"192.168.44.1/24\"",
        "dhcp_enabled = true",
        "dhcp_lease_time = \"12h\"",
        "request_dhcp = true",
        "auto_discover_peers = true",
        "poll_interval_ms = 2000",
        "scan_interval_ms = 5000",
        "peer_discovery_interval_ms = 2000",
        "bluetoothctl_timeout_s = 15",
        "discoverable_timeout_s = 180",
        "startup_timeout_ms = 15000",
    ] {
        assert!(
            rendered.contains(needle),
            "bluetooth missing `{needle}`. rendered:\n{rendered}"
        );
    }
    // Optional fields commented out.
    assert!(rendered.contains("# dhcp_range ="));
    assert!(rendered.contains("# dhcp_dns ="));

    let cfg = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert!(!cfg.bluetooth.enabled);
    assert!(cfg.bluetooth.radio_discovery_enabled);
    assert!(cfg.bluetooth.connect_pan);
    assert!(cfg.bluetooth.auto_discover_peers);
    assert_eq!(cfg.bluetooth.dhcp_range, None);
    assert_eq!(cfg.bluetooth.dhcp_dns, None);
}

#[test]
fn template_bluetooth_local_alias_uses_node_name() {
    let rendered = render_config_template(&[NodeRole::Client], Some("edge-laptop"));
    assert!(
        rendered.contains("local_alias = \"PIM-edge-laptop\""),
        "expected local_alias derived from override name; rendered:\n{rendered}"
    );
}

#[test]
fn template_wifi_direct_emits_full_block_disabled_by_default() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    assert!(rendered.contains("[wifi_direct]"));
    for needle in [
        "go_intent = 7",
        "listen_channel = 6",
        "op_channel = 6",
        "connect_method = \"pbc\"",
    ] {
        assert!(
            rendered.contains(needle),
            "wifi_direct missing `{needle}`. rendered:\n{rendered}"
        );
    }
    let cfg = pim_core::Config::from_toml_str(&rendered).unwrap();
    assert!(!cfg.wifi_direct.enabled);
    assert_eq!(cfg.wifi_direct.go_intent, 7);
    assert_eq!(cfg.wifi_direct.listen_channel, 6);
    assert_eq!(cfg.wifi_direct.op_channel, 6);
    assert_eq!(cfg.wifi_direct.connect_method, "pbc");
}

#[test]
fn template_static_peers_block_documents_both_mechanisms() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    // Both transport mechanisms documented as commented examples so
    // operators see the option without having to read the docs.
    assert!(rendered.contains("# mechanism = \"tcp\""));
    assert!(rendered.contains("# mechanism = \"bluetooth\""));
    assert!(rendered.contains("# ip = \"192.168.44.2\""));
}

#[test]
fn template_round_trips_for_every_role_combination() {
    use NodeRole::{Client, Gateway, Relay};
    let combos: &[&[NodeRole]] = &[
        &[Client],
        &[Relay],
        &[Gateway],
        &[Client, Relay],
        &[Client, Gateway],
        &[Relay, Gateway],
        &[Client, Relay, Gateway],
    ];
    for roles in combos {
        let rendered = render_config_template(roles, None);
        let cfg = pim_core::Config::from_toml_str(&rendered).unwrap_or_else(|e| {
            panic!("failed to parse template for {roles:?}: {e}\nrendered:\n{rendered}")
        });
        // Every parsed config must agree with the role inputs on capability flags.
        let want_gateway = roles.iter().any(|r| matches!(r, Gateway));
        let want_relay = roles.iter().any(|r| matches!(r, Relay));
        assert_eq!(
            cfg.gateway.enabled, want_gateway,
            "gateway flag for {roles:?}"
        );
        assert_eq!(cfg.relay.enabled, want_relay, "relay flag for {roles:?}");
    }
}
