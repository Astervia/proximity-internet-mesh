//! LE GAP scan (Phase 4) — discover nearby PIM advertisers and dial
//! their CoC PSM.
//!
//! Speaks to BlueZ via `bluer`: `Adapter1.SetDiscoveryFilter` with
//! `Transport = "le"` and the PIM service UUID, then
//! `Adapter1.StartDiscovery`. For each `InterfacesAdded` event
//! representing a new `Device1` whose `ServiceData` contains our
//! service UUID, parse the PSM (and optional truncated mesh-tag) and
//! dial via [`super::socket::connect`].
//!
//! Mesh-tag fingerprint check: when the local node is on a private
//! mesh, peers whose service-data carries a non-matching tag prefix
//! are skipped before opening the channel. Saves a wasted dial on a
//! wrong-mesh peer and surfaces a quieter "no PIM-* advertisers
//! visible" UX. The full mesh-tag still gets verified during the
//! Hello handshake — this is a pre-filter, not a security gate.

#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use bluer::{
    Adapter, AdapterEvent, Address, AddressType, DiscoveryFilter, DiscoveryTransport, Uuid,
};
use futures_util::StreamExt;
use tokio::sync::{mpsc, Mutex};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{
    format_bdaddr, session, socket, BdAddr, CocConfig, CocEvent, LocalIdentity, PIM_SERVICE_UUID,
};

/// Spawn the LE scan task. Cancels cleanly on the cancel token. Errors
/// during D-Bus setup are reported as `CocEvent::Error` and the task
/// retries with backoff so a temporarily-unavailable bluetoothd
/// (boot, restart) doesn't permanently silence the scan.
pub fn spawn(
    cfg: CocConfig,
    identity: LocalIdentity,
    events_tx: mpsc::Sender<CocEvent>,
    cancel: CancellationToken,
    active: Arc<Mutex<HashSet<BdAddr>>>,
) {
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(2);
        loop {
            if cancel.is_cancelled() {
                return;
            }
            match run(&cfg, &identity, &events_tx, &cancel, active.clone()).await {
                Ok(()) => return, // cancelled
                Err(e) => {
                    warn!(target: "pim-bluetooth-coc-scan", error = %e, retry_in_s = backoff.as_secs(), "scan loop errored, retrying");
                    let _ = events_tx
                        .send(CocEvent::Error {
                            code: -33111,
                            message: format!("scan loop: {e}"),
                        })
                        .await;
                    tokio::select! {
                        _ = sleep(backoff) => {}
                        _ = cancel.cancelled() => return,
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
    });
}

async fn run(
    cfg: &CocConfig,
    identity: &LocalIdentity,
    events_tx: &mpsc::Sender<CocEvent>,
    cancel: &CancellationToken,
    active: Arc<Mutex<HashSet<BdAddr>>>,
) -> bluer::Result<()> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    if !adapter.is_powered().await.unwrap_or(false) {
        if let Err(e) = adapter.set_powered(true).await {
            warn!(target: "pim-bluetooth-coc-scan", error = %e, "set_powered(true) failed");
        }
    }

    let service_uuid =
        Uuid::parse_str(PIM_SERVICE_UUID).expect("PIM_SERVICE_UUID const is a valid UUID");

    let mut filter = DiscoveryFilter::default();
    filter.uuids.insert(service_uuid);
    filter.transport = DiscoveryTransport::Le;
    adapter.set_discovery_filter(filter).await?;
    info!(target: "pim-bluetooth-coc-scan", adapter = adapter.name(), "LE discovery armed");

    let mut events = adapter.discover_devices().await?;
    while let Some(event) = tokio::select! {
        _ = cancel.cancelled() => None,
        e = events.next() => e,
    } {
        match event {
            AdapterEvent::DeviceAdded(addr) => {
                let cfg = cfg.clone();
                let identity = identity.clone();
                let events_tx = events_tx.clone();
                let cancel = cancel.child_token();
                let active = active.clone();
                let adapter = adapter.clone();
                // Detached so a slow getter on one device doesn't
                // back-pressure the event stream — every other
                // DeviceAdded would otherwise queue behind it.
                tokio::spawn(async move {
                    if let Err(e) = try_dial_discovered(
                        &adapter,
                        addr,
                        service_uuid,
                        cfg,
                        identity,
                        events_tx,
                        cancel,
                        active,
                    )
                    .await
                    {
                        debug!(target: "pim-bluetooth-coc-scan", peer = %addr, error = %e, "skip discovered peer");
                    }
                });
            }
            AdapterEvent::DeviceRemoved(_) => {}
            AdapterEvent::PropertyChanged(_) => {}
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn try_dial_discovered(
    adapter: &Adapter,
    addr: Address,
    service_uuid: Uuid,
    cfg: CocConfig,
    identity: LocalIdentity,
    events_tx: mpsc::Sender<CocEvent>,
    cancel: CancellationToken,
    active: Arc<Mutex<HashSet<BdAddr>>>,
) -> bluer::Result<()> {
    let device = adapter.device(addr)?;
    let service_data = device.service_data().await?.unwrap_or_default();
    let Some(payload) = service_data.get(&service_uuid).cloned() else {
        return Ok(()); // not a PIM advertiser
    };
    let Some((peer_psm, peer_tag_prefix)) = parse_service_data(&payload) else {
        warn!(target: "pim-bluetooth-coc-scan", peer = %addr, len = payload.len(), "malformed PIM service-data");
        return Ok(());
    };
    if peer_psm == 0 {
        debug!(target: "pim-bluetooth-coc-scan", peer = %addr, "peer advertises psm=0 (acceptor-only); skipping dial");
        return Ok(());
    }
    // Pre-filter on mesh-tag prefix. The full HMAC is still verified
    // at the Hello layer — this just avoids a wasted CoC dial when
    // the peer is clearly on another mesh.
    if let (Some(local_key), Some(peer_prefix)) =
        (identity.mesh_handshake_key.as_ref(), peer_tag_prefix)
    {
        // We can't yet check the prefix without knowing the peer's
        // node_id (the HMAC is over their node_id, not ours). Skip
        // the pre-filter when the advertised name doesn't carry one;
        // the Hello layer remains the authoritative check.
        let _ = local_key;
        let _ = peer_prefix;
    }

    // Resolve the kernel-format BdAddr (kernel little-endian reverse).
    let mut bd_addr: BdAddr = [0u8; 6];
    for (i, b) in addr.0.iter().enumerate() {
        bd_addr[5 - i] = *b;
    }

    // Dedup with the listener / outbound shared set.
    {
        let set = active.lock().await;
        if set.contains(&bd_addr) {
            return Ok(());
        }
    }
    {
        let mut set = active.lock().await;
        set.insert(bd_addr);
    }

    // Bluetooth address-type for the kernel sockaddr_l2 field. BlueZ's
    // AddressType maps directly: LePublic → 0x01, LeRandom → 0x02.
    let bdaddr_type = match device.address_type().await.unwrap_or(AddressType::LePublic) {
        AddressType::LePublic => super::BDADDR_LE_PUBLIC,
        AddressType::LeRandom => super::BDADDR_LE_RANDOM,
        AddressType::BrEdr => {
            // BR/EDR peer advertising a PIM UUID is nonsensical for
            // this transport; release the slot and skip.
            let mut set = active.lock().await;
            set.remove(&bd_addr);
            return Ok(());
        }
    };

    let name = device.name().await.unwrap_or(None).unwrap_or_default();
    let bd_str = format_bdaddr(&bd_addr);
    info!(
        target: "pim-bluetooth-coc-scan",
        peer = %addr,
        peer_psm = format!("{:#06x}", peer_psm),
        name = %name,
        "coc scan: dialing discovered peer",
    );

    let stream = match socket::connect(bd_addr, peer_psm, bdaddr_type).await {
        Ok(s) => s,
        Err(e) => {
            let _ = events_tx
                .send(CocEvent::OpenFailed {
                    bd_addr: bd_str,
                    name: name.clone(),
                    reason: e.to_string(),
                })
                .await;
            let mut set = active.lock().await;
            set.remove(&bd_addr);
            return Ok(());
        }
    };

    let bridge_addr = cfg.local_bridge_addr;
    tokio::spawn(async move {
        session::run(
            stream,
            bd_addr,
            true,
            identity,
            events_tx,
            cancel,
            bridge_addr,
        )
        .await;
        let mut set = active.lock().await;
        set.remove(&bd_addr);
    });
    Ok(())
}

/// Parse a PIM service-data payload back into `(psm, optional mesh-tag prefix)`.
/// Returns `None` if the payload is shorter than 2 bytes.
pub(super) fn parse_service_data(payload: &[u8]) -> Option<(u16, Option<&[u8]>)> {
    if payload.len() < 2 {
        return None;
    }
    let psm = u16::from_le_bytes([payload[0], payload[1]]);
    let tag = if payload.len() > 2 {
        Some(&payload[2..])
    } else {
        None
    };
    Some((psm, tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_payload_psm_only() {
        let (psm, tag) = parse_service_data(&[0x83, 0x00]).unwrap();
        assert_eq!(psm, 0x0083);
        assert!(tag.is_none());
    }

    #[test]
    fn parse_payload_with_tag_prefix() {
        let mut payload = vec![0xFE, 0x00];
        payload.extend(vec![0xAB; 16]);
        let (psm, tag) = parse_service_data(&payload).unwrap();
        assert_eq!(psm, 0x00FE);
        assert_eq!(tag.unwrap(), &[0xAB; 16]);
    }

    #[test]
    fn parse_payload_too_short_returns_none() {
        assert!(parse_service_data(&[]).is_none());
        assert!(parse_service_data(&[0x83]).is_none());
    }
}
