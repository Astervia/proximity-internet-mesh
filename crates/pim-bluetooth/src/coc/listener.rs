//! Acceptor loop: bind L2CAP CoC PSM, accept inbound, spawn session.
//!
//! One-to-one with `rfcomm/listener.rs` — only the underlying socket
//! type changes. The active-session dedup `HashSet<BdAddr>` is shared
//! with `outbound::spawn` so first-arriver wins.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::socket::CocListener;
use super::{session, BdAddr, CocConfig, CocError, CocEvent, LocalIdentity};

/// Spawn the acceptor task. Errors at bind time are propagated;
/// runtime errors are surfaced as `CocEvent::Error` and the task
/// continues.
pub fn spawn(
    cfg: CocConfig,
    identity: LocalIdentity,
    events_tx: mpsc::Sender<CocEvent>,
    cancel: CancellationToken,
    active: Arc<Mutex<std::collections::HashSet<BdAddr>>>,
) -> Result<(), CocError> {
    let listener = CocListener::bind(cfg.psm).map_err(|e| CocError::BindFailed {
        psm: cfg.psm,
        source: e,
    })?;
    info!(target: "pim-bluetooth-coc", psm = format!("{:#06x}", cfg.psm), "listening");

    let tx0 = events_tx.clone();
    let psm = cfg.psm;
    tokio::spawn(async move {
        let _ = tx0.send(CocEvent::Listening { psm }).await;
    });

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!(target: "pim-bluetooth-coc", "acceptor shutdown");
                    return;
                }
                r = listener.accept() => match r {
                    Ok((stream, peer_addr)) => {
                        {
                            let mut set = active.lock().await;
                            if set.contains(&peer_addr) {
                                continue;
                            }
                            set.insert(peer_addr);
                        }
                        let identity = identity.clone();
                        let events_tx = events_tx.clone();
                        // Child token — same rationale as the RFCOMM
                        // acceptor: bridge's per-session cancel must not
                        // bubble back up and kill the acceptor itself.
                        let cancel = cancel.child_token();
                        let active = active.clone();
                        let bridge_addr = cfg.local_bridge_addr;
                        tokio::spawn(async move {
                            session::run(
                                stream,
                                peer_addr,
                                false,
                                identity,
                                events_tx,
                                cancel,
                                bridge_addr,
                            )
                            .await;
                            let mut set = active.lock().await;
                            set.remove(&peer_addr);
                        });
                    }
                    Err(e) => {
                        warn!(target: "pim-bluetooth-coc", "accept failed: {e}");
                        let _ = events_tx
                            .send(CocEvent::Error {
                                code: -33120,
                                message: format!("accept failed: {e}"),
                            })
                            .await;
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                },
            }
        }
    });

    Ok(())
}
