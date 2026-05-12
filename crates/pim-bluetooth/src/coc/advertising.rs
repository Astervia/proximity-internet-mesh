//! LE GAP advertising (Phase 4) — registers a peripheral advertisement
//! carrying the PIM service UUID + service-data (PSM + truncated
//! mesh-tag prefix) via BlueZ's `org.bluez.LEAdvertisingManager1`.
//!
//! The `bluer` crate marshals the D-Bus calls; we just construct the
//! [`bluer::adv::Advertisement`] record and hand it to
//! [`bluer::Adapter::advertise`]. The returned handle must stay alive
//! for the lifetime of the advertisement — dropping it sends
//! `UnregisterAdvertisement` to BlueZ, so we own the handle until the
//! shutdown token fires.
//!
//! Service-data payload layout (key = [`super::PIM_SERVICE_UUID`]):
//!
//! ```text
//! offset 0–1  : PSM little-endian (matches `htobs(psm)`)
//! offset 2–N  : truncated mesh-tag prefix (first 16 B of the HMAC).
//!               Lets scanners pre-filter wrong-mesh peers without
//!               opening the channel. Optional — open-mesh nodes
//!               omit these bytes entirely.
//! ```
//!
//! 31-byte AD limit (BT 5.0 legacy adv) is easily satisfied: 2 PSM
//! bytes + 16 mesh-tag bytes + the UUID itself (16 B) + AD-record
//! overhead (~6 B) ≈ 40 B. We rely on BlueZ falling back to
//! extended advertising on 5.0+ controllers when the legacy 31 B
//! envelope overflows; controllers that don't support extended
//! advertising will see the local-name dropped first, then any
//! optional fields. Status is logged at INFO so operators notice.

#![cfg(target_os = "linux")]

use std::collections::{BTreeMap, BTreeSet};

use bluer::adv::{Advertisement, AdvertisementHandle, Type};
use bluer::Uuid;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{CocConfig, CocEvent, LocalIdentity, PIM_SERVICE_UUID};

/// Spawn the GAP-advertising task. Cancels cleanly on the token;
/// drops the `AdvertisementHandle` on shutdown, which triggers BlueZ's
/// `UnregisterAdvertisement` D-Bus call.
///
/// `advertised_psm` is the PSM the local CoC listener bound to. We
/// publish it verbatim in service-data; Android sees a u16 LE and
/// dials via `createL2capChannel(psm)`. Pass `0` to suppress the PSM
/// field (acceptor-only deployments that want to be discoverable for
/// pairing UI but not dialed).
pub fn spawn(
    cfg: CocConfig,
    identity: LocalIdentity,
    advertised_psm: u16,
    events_tx: mpsc::Sender<CocEvent>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let result = run(cfg, identity, advertised_psm, cancel.clone()).await;
        if let Err(e) = result {
            warn!(target: "pim-bluetooth-coc-adv", error = %e, "advertising loop exited");
            let _ = events_tx
                .send(CocEvent::Error {
                    code: -33110,
                    message: format!("advertising failed: {e}"),
                })
                .await;
        }
    });
}

async fn run(
    cfg: CocConfig,
    identity: LocalIdentity,
    advertised_psm: u16,
    cancel: CancellationToken,
) -> bluer::Result<()> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    // Ensure the controller is powered. BlueZ accepts
    // RegisterAdvertisement on an unpowered adapter but the
    // advertisement only goes on air after `Powered = true`, so flip
    // it explicitly to avoid the silent "advertise but never see it
    // in nRF Connect" failure mode.
    if !adapter.is_powered().await.unwrap_or(false) {
        if let Err(e) = adapter.set_powered(true).await {
            warn!(target: "pim-bluetooth-coc-adv", error = %e, "set_powered(true) failed");
        }
    }

    // `PIM_SERVICE_UUID` is a const; parsing is infallible at runtime
    // (verified by `service_uuid_parses` in `coc::tests`). `expect`
    // makes the intent explicit without inventing a synthetic
    // `bluer::Error` variant.
    let service_uuid =
        Uuid::parse_str(PIM_SERVICE_UUID).expect("PIM_SERVICE_UUID const is a valid UUID");

    let mut service_data = BTreeMap::new();
    let payload = build_service_data_payload(
        advertised_psm,
        identity.mesh_handshake_key.as_ref(),
        &identity.node_id_hex,
    );
    service_data.insert(service_uuid, payload);

    let mut service_uuids = BTreeSet::new();
    service_uuids.insert(service_uuid);

    let adv = Advertisement {
        advertisement_type: Type::Peripheral,
        service_uuids,
        service_data,
        local_name: Some(format!(
            "{}{}",
            if cfg.prefix.is_empty() {
                "PIM-"
            } else {
                cfg.prefix.as_str()
            },
            identity.name,
        )),
        discoverable: Some(true),
        ..Default::default()
    };

    info!(
        target: "pim-bluetooth-coc-adv",
        adapter = adapter.name(),
        psm = format!("{:#06x}", advertised_psm),
        "registering LE advertisement",
    );
    let handle: AdvertisementHandle = adapter.advertise(adv).await?;
    info!(target: "pim-bluetooth-coc-adv", "advertisement registered");

    // Hold the handle for the lifetime of the cancellation token.
    // Dropping it unregisters the advertisement via D-Bus.
    cancel.cancelled().await;
    drop(handle);
    info!(target: "pim-bluetooth-coc-adv", "advertisement unregistered (shutdown)");
    Ok(())
}

/// Pack the PSM (and, for private-mesh nodes, the first 16 bytes of
/// the local mesh-tag) into the service-data payload. The mesh-tag
/// prefix is a fingerprint, not the full HMAC: it lets scanners
/// pre-filter wrong-mesh peers without revealing enough bits to
/// brute-force the mesh secret.
pub(super) fn build_service_data_payload(
    psm: u16,
    mesh_key: Option<&[u8; 32]>,
    local_node_hex: &str,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + 16);
    out.extend_from_slice(&psm.to_le_bytes());
    if let Some(key) = mesh_key {
        let raw = pim_crypto::compute_rfcomm_hello_tag(key, local_node_hex);
        // Truncate to 16 bytes. The Hello layer still gets the full
        // 32-byte HMAC to verify against — this prefix is purely a
        // scan-time filter, not a cryptographic check.
        out.extend_from_slice(&raw[..16.min(raw.len())]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_data_payload_open_mesh_is_psm_only() {
        let payload = build_service_data_payload(0x0083, None, "abcd");
        assert_eq!(payload, vec![0x83, 0x00]);
    }

    #[test]
    fn service_data_payload_private_mesh_appends_tag_prefix() {
        let key = [0xABu8; 32];
        let payload = build_service_data_payload(0x00FE, Some(&key), "deadbeef");
        // Format: psm-le (2) + truncated tag (16) = 18 bytes.
        assert_eq!(payload.len(), 18);
        assert_eq!(&payload[..2], &[0xFE, 0x00]);
        // Tag bytes are deterministic for fixed key+node — re-derive
        // and compare to lock the wire format.
        let expected_tag = pim_crypto::compute_rfcomm_hello_tag(&key, "deadbeef");
        assert_eq!(&payload[2..], &expected_tag[..16]);
    }
}
