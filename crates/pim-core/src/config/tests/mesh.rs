use super::super::*;
use super::{FULL_CONFIG, MINIMAL_CONFIG};

#[test]
fn mesh_defaults_when_section_absent() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert_eq!(config.mesh.mode, MeshMode::Open);
    assert!(config.mesh.passphrase.is_none());
    assert!(config.mesh.mesh_id.is_none());
    assert_eq!(config.mesh.kdf.m_cost_kib, 65536);
    assert_eq!(config.mesh.kdf.t_cost, 3);
    assert_eq!(config.mesh.kdf.p_cost, 1);
}

#[test]
fn mesh_full_config_round_trips() {
    let config = Config::from_toml_str(FULL_CONFIG).unwrap();
    assert_eq!(config.mesh.mode, MeshMode::Private);
    assert_eq!(
        config.mesh.passphrase.as_deref(),
        Some("correct horse battery staple")
    );
    assert_eq!(config.mesh.mesh_id.as_deref(), Some("test-mesh"));
    assert_eq!(config.mesh.kdf.m_cost_kib, 8);
    assert_eq!(config.mesh.kdf.t_cost, 1);
    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert_eq!(config, reparsed);
}

#[test]
fn mesh_open_with_just_mode_string() {
    let toml = r#"
[node]
name = "t"
[mesh]
mode = "open"
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.mesh.mode, MeshMode::Open);
}

#[test]
fn mesh_kdf_partial_override() {
    // Only m_cost_kib overridden — others should fall back to defaults.
    let toml = r#"
[node]
name = "t"
[mesh]
mode = "private"
passphrase = "x"
[mesh.kdf]
m_cost_kib = 16
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.mesh.kdf.m_cost_kib, 16);
    assert_eq!(config.mesh.kdf.t_cost, 3);
    assert_eq!(config.mesh.kdf.p_cost, 1);
}
