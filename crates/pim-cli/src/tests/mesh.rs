use super::super::*;
use pim_core::{Config, MeshMode};

const OPEN_TOML: &str = r#"
[node]
name = "open-node"
"#;

const OPEN_WITH_LABEL_TOML: &str = r#"
[node]
name = "labelled-open"
[mesh]
mode = "open"
mesh_id = "office"
"#;

const PRIVATE_TOML: &str = r#"
[node]
name = "private-node"
[mesh]
mode = "private"
passphrase = "correct horse battery staple"
mesh_id = "office"
[mesh.kdf]
m_cost_kib = 8
t_cost = 1
p_cost = 1
"#;

const PRIVATE_NO_PASSPHRASE_TOML: &str = r#"
[node]
name = "broken-private"
[mesh]
mode = "private"
[mesh.kdf]
m_cost_kib = 8
t_cost = 1
p_cost = 1
"#;

#[test]
fn open_report_has_no_fingerprint() {
    let cfg = Config::from_toml_str(OPEN_TOML).unwrap();
    let report = build_mesh_status_report(&cfg).unwrap();
    assert_eq!(report.mode, MeshMode::Open);
    assert!(report.fingerprint_hex.is_none());
    assert!(report.mesh_id.is_none());
}

#[test]
fn open_report_keeps_cosmetic_label() {
    let cfg = Config::from_toml_str(OPEN_WITH_LABEL_TOML).unwrap();
    let report = build_mesh_status_report(&cfg).unwrap();
    assert_eq!(report.mode, MeshMode::Open);
    assert_eq!(report.mesh_id.as_deref(), Some("office"));
    assert!(report.fingerprint_hex.is_none());
}

#[test]
fn private_report_includes_fingerprint() {
    let cfg = Config::from_toml_str(PRIVATE_TOML).unwrap();
    let report = build_mesh_status_report(&cfg).unwrap();
    assert_eq!(report.mode, MeshMode::Private);
    assert_eq!(report.mesh_id.as_deref(), Some("office"));
    let fp = report.fingerprint_hex.as_deref().unwrap();
    assert_eq!(fp.len(), 16, "fingerprint must be 8 bytes hex-encoded");
}

#[test]
fn private_fingerprint_matches_kernel_kdf() {
    // CLI and daemon must produce identical fingerprints from identical
    // inputs — otherwise the operator can't match the UI's value to the
    // daemon's startup log.
    let cfg = Config::from_toml_str(PRIVATE_TOML).unwrap();
    let report = build_mesh_status_report(&cfg).unwrap();

    let direct = pim_crypto::MeshSecret::derive(
        cfg.mesh.passphrase.as_deref().unwrap(),
        cfg.mesh.mesh_id.as_deref(),
        pim_crypto::MeshKdfParams {
            m_cost_kib: cfg.mesh.kdf.m_cost_kib,
            t_cost: cfg.mesh.kdf.t_cost,
            p_cost: cfg.mesh.kdf.p_cost,
        },
    )
    .unwrap();
    assert_eq!(report.fingerprint_hex.unwrap(), direct.fingerprint_hex());
}

#[test]
fn private_without_passphrase_errors() {
    let cfg = Config::from_toml_str(PRIVATE_NO_PASSPHRASE_TOML).unwrap();
    let err = build_mesh_status_report(&cfg).unwrap_err();
    assert!(
        format!("{err}").contains("passphrase"),
        "error must mention passphrase: {err}"
    );
}
