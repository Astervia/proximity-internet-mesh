use super::super::*;
use std::path::PathBuf;

#[test]
fn authorization_policy_round_trips() {
    let toml = r#"
[node]
name = "t"
[security]
authorization_policy = "trust_on_first_use"
authorized_peers = ["abababababababababababababababab"]
trust_store_file = "/tmp/pim/trusted-peers.toml"
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(
        config.security.authorization_policy,
        AuthorizationPolicy::TrustOnFirstUse
    );
    assert_eq!(config.security.authorized_peers.len(), 1);
    assert_eq!(
        config.security.trust_store_file,
        PathBuf::from("/tmp/pim/trusted-peers.toml")
    );
    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert_eq!(config, reparsed);
}
