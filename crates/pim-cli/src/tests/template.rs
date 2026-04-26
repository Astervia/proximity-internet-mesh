use super::super::*;

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
