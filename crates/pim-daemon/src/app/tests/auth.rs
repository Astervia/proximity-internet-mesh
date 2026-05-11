use super::super::*;
use super::peer_id;

fn temp_trust_store_path() -> PathBuf {
    std::env::temp_dir().join(format!("pim-trust-{}.toml", rand::random::<u64>()))
}

#[tokio::test]
async fn authorization_allow_list_rejects_unlisted_peer() {
    let path = temp_trust_store_path();
    let manager =
        AuthorizationManager::new(AuthorizationPolicy::AllowList, [peer_id(1)], path.clone())
            .unwrap();
    assert!(manager.authorize_discovered_peer(peer_id(1)).await);
    assert!(!manager.authorize_discovered_peer(peer_id(2)).await);
    assert_eq!(
        manager
            .authorize_authenticated_peer(peer_id(2))
            .await
            .unwrap(),
        AuthorizationDecision::Rejected
    );
    std::fs::remove_file(path).ok();
}

#[tokio::test]
async fn authorization_tofu_persists_new_peer() {
    let path = temp_trust_store_path();
    let manager =
        AuthorizationManager::new(AuthorizationPolicy::TrustOnFirstUse, [], path.clone()).unwrap();
    assert_eq!(
        manager
            .authorize_authenticated_peer(peer_id(7))
            .await
            .unwrap(),
        AuthorizationDecision::TrustedOnFirstUse
    );

    let reloaded =
        AuthorizationManager::new(AuthorizationPolicy::TrustOnFirstUse, [], path.clone()).unwrap();
    assert_eq!(
        reloaded
            .authorize_authenticated_peer(peer_id(7))
            .await
            .unwrap(),
        AuthorizationDecision::Allowed
    );
    std::fs::remove_file(path).ok();
}
