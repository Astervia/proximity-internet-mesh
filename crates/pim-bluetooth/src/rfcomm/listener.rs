//! Acceptor loop: bind RFCOMM channel, accept inbound, spawn session.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::socket::RfcommListener;
use super::{session, BdAddr, LocalIdentity, RfcommConfig, RfcommError, RfcommEvent};

/// Spawn the acceptor task. Errors at bind time are propagated; runtime
/// errors are surfaced as `RfcommEvent::Error` and the task continues.
///
/// `active` is shared with `outbound::spawn` so the inbound accept and
/// the outbound dial cooperate on dedup — first-arriving wins, the
/// other path skips its session for the same peer.
pub fn spawn(
    cfg: RfcommConfig,
    identity: LocalIdentity,
    events_tx: mpsc::Sender<RfcommEvent>,
    cancel: CancellationToken,
    active: Arc<Mutex<std::collections::HashSet<BdAddr>>>,
) -> Result<(), RfcommError> {
    let listener = RfcommListener::bind(cfg.channel).map_err(|e| RfcommError::BindFailed {
        channel: cfg.channel,
        source: e,
    })?;
    info!(target: "pim-bluetooth-rfcomm", channel = cfg.channel, "listening");

    // Notify the daemon we're up so the UI can mark the service alive.
    let tx0 = events_tx.clone();
    let ch = cfg.channel;
    tokio::spawn(async move {
        let _ = tx0.send(RfcommEvent::Listening { channel: ch }).await;
    });

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!(target: "pim-bluetooth-rfcomm", "acceptor shutdown");
                    return;
                }
                r = listener.accept() => match r {
                    Ok((stream, peer_addr)) => {
                        // Dedup: if outbound already opened a session
                        // with this peer, drop the inbound channel.
                        {
                            let mut set = active.lock().await;
                            if set.contains(&peer_addr) {
                                continue;
                            }
                            set.insert(peer_addr);
                        }
                        let identity = identity.clone();
                        let events_tx = events_tx.clone();
                        // Defense in depth: hand the session a CHILD
                        // token. The bridge inside `session::run` may
                        // cancel its own token to tear down its r2t/t2r
                        // pumps (see session.rs) — if that token were
                        // a clone of `cancel`, the cancellation would
                        // bubble up and shut down THIS acceptor task.
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
                        warn!(target: "pim-bluetooth-rfcomm", "accept failed: {e}");
                        let _ = events_tx
                            .send(RfcommEvent::Error {
                                code: -33020,
                                message: format!("accept failed: {e}"),
                            })
                            .await;
                        // Brief backoff to avoid hot-looping on repeated EBADF.
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                },
            }
        }
    });

    let _ = error::<()>; // suppress unused-import warning when log macros change
    Ok(())
}

#[allow(dead_code)]
fn error<T>() {}
