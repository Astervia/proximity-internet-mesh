use super::super::*;
use super::peer_id;

fn temp_trust_store_path() -> PathBuf {
    std::env::temp_dir().join(format!("pim-trust-{}.toml", rand::random::<u64>()))
}

#[test]
fn ip_request_classifier_accepts_first_request() {
    let requester = peer_id(70);
    let mut pending = HashSet::new();
    assert_eq!(
        classify_ip_request(&mut pending, requester, requester),
        IpRequestDisposition::Process
    );
    assert!(pending.contains(&requester));
}

#[test]
fn ip_request_classifier_drops_duplicate_inflight_request() {
    let requester = peer_id(71);
    let mut pending = HashSet::from([requester]);
    assert_eq!(
        classify_ip_request(&mut pending, requester, requester),
        IpRequestDisposition::DuplicateInFlight
    );
}

#[test]
fn ip_request_classifier_rejects_spoofed_requester() {
    let requester = peer_id(72);
    let from_peer = peer_id(73);
    let mut pending = HashSet::new();
    assert_eq!(
        classify_ip_request(&mut pending, requester, from_peer),
        IpRequestDisposition::SpoofedRequester
    );
    assert!(pending.is_empty());
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
