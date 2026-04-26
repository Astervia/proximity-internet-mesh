use super::*;

#[test]
fn parse_stats_str_extracts_key_value_pairs() {
    let input = "peers=3\nroutes=5\npackets_forwarded=100\nbytes_forwarded=51200\n";
    let pairs = parse_stats_str(input);
    assert_eq!(pairs.len(), 4);
    assert_eq!(pairs[0], ("peers".to_string(), "3".to_string()));
    assert_eq!(pairs[1], ("routes".to_string(), "5".to_string()));
    assert_eq!(
        pairs[2],
        ("packets_forwarded".to_string(), "100".to_string())
    );
    assert_eq!(
        pairs[3],
        ("bytes_forwarded".to_string(), "51200".to_string())
    );
}

#[test]
fn parse_stats_str_skips_malformed_lines() {
    let input = "peers=3\nnot-a-pair\nbytes_forwarded=512\n";
    let pairs = parse_stats_str(input);
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs[0].0, "peers");
    assert_eq!(pairs[1].0, "bytes_forwarded");
}

#[test]
fn parse_stats_str_empty_input() {
    let pairs = parse_stats_str("");
    assert!(pairs.is_empty());
}

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
fn gateway_ip_from_static_mesh_cidr_uses_first_host() {
    assert_eq!(
        gateway_ip_from_config_mesh_ip("10.77.0.42/24"),
        Some(Ipv4Addr::new(10, 77, 0, 1))
    );
}

#[test]
fn gateway_ip_from_auto_mesh_ip_is_unknown() {
    assert_eq!(gateway_ip_from_config_mesh_ip("auto"), None);
}

#[test]
#[cfg(target_os = "linux")]
fn route_present_matches_split_default_route() {
    let routes = "0.0.0.0/1 via 10.77.0.1 dev pim0 onlink\n";
    assert!(route_present_linux(
        routes,
        "0.0.0.0/1",
        Ipv4Addr::new(10, 77, 0, 1),
        "pim0"
    ));
}

#[test]
fn config_template_uses_platform_interface_name() {
    let rendered = render_config_template(&[NodeRole::Client], None);
    assert!(rendered.contains(&format!("name = {:?}", default_interface_name())));
}
