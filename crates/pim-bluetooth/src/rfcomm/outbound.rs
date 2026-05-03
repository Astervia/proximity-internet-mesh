//! Outbound discovery loop: scan paired devices via `bluetoothctl`,
//! filter by prefix, dial RFCOMM channel for each new candidate.

#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::sync::Arc;

use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::socket;
use super::{parse_bdaddr, session, BdAddr, LocalIdentity, RfcommConfig, RfcommEvent};

/// Spawn the outbound discovery task. Cancels cleanly on the cancel
/// token; never returns errors directly (errors become RfcommEvent).
///
/// `active` is shared with `listener::spawn` so the inbound accept and
/// the outbound dial cooperate on dedup. Without that, simultaneous
/// poll cycles on both peers' daemons each insert into their own local
/// set, both dial through, both accept the inbound, and the resulting
/// duplicate sessions clobber each other in `register_peer`.
pub fn spawn(
    cfg: RfcommConfig,
    identity: LocalIdentity,
    events_tx: mpsc::Sender<RfcommEvent>,
    cancel: CancellationToken,
    active: Arc<Mutex<HashSet<BdAddr>>>,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!(target: "pim-bluetooth-rfcomm", "outbound shutdown");
                    return;
                }
                _ = sleep(cfg.poll_interval) => {}
            }

            let paired = match scan_paired_devices(&cfg.bluetoothctl_command, &cfg.prefix).await {
                Ok(p) => p,
                Err(e) => {
                    warn!(target: "pim-bluetooth-rfcomm", "scan failed: {e}");
                    continue;
                }
            };
            // Diagnostic — without this it's impossible to tell whether
            // the outbound loop is alive at all. Logged once per poll
            // tick at INFO so the bench operator sees the cadence.
            info!(
                target: "pim-bluetooth-rfcomm",
                paired_count = paired.len(),
                "rfcomm outbound poll"
            );
            for (bd_addr_str, name) in paired {
                let bd_addr = match parse_bdaddr(&bd_addr_str) {
                    Some(a) => a,
                    None => continue,
                };
                {
                    let set = active.lock().await;
                    if set.contains(&bd_addr) {
                        continue;
                    }
                }
                {
                    let mut set = active.lock().await;
                    set.insert(bd_addr);
                }
                info!(
                    target: "pim-bluetooth-rfcomm",
                    bd_addr = %bd_addr_str,
                    name = %name,
                    channel = cfg.channel,
                    "rfcomm dialing peer"
                );
                let stream = match socket::connect(bd_addr, cfg.channel).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = events_tx
                            .send(RfcommEvent::OpenFailed {
                                bd_addr: bd_addr_str.clone(),
                                name: name.clone(),
                                reason: e.to_string(),
                            })
                            .await;
                        let mut set = active.lock().await;
                        set.remove(&bd_addr);
                        continue;
                    }
                };
                let identity = identity.clone();
                let events_tx = events_tx.clone();
                let cancel = cancel.clone();
                let active = active.clone();
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
            }
        }
    });
}

/// Run `bluetoothctl devices Paired` and parse output. Each line:
/// `Device AA:BB:CC:DD:EE:FF Name With Spaces`. Filter to those whose
/// name starts with `prefix`.
async fn scan_paired_devices(
    bluetoothctl_command: &std::path::Path,
    prefix: &str,
) -> std::io::Result<Vec<(String, String)>> {
    let out = Command::new(bluetoothctl_command)
        .args(["devices", "Paired"])
        .output()
        .await?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "bluetoothctl exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(parse_bluetoothctl_devices(&stdout, prefix))
}

/// Parse `bluetoothctl devices [Paired]` output into a list of
/// `(bd_addr, name)` pairs filtered by `prefix`. Pulled out for unit test.
pub(crate) fn parse_bluetoothctl_devices(stdout: &str, prefix: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if !line.starts_with("Device ") {
            continue;
        }
        // "Device AA:BB:CC:DD:EE:FF Some Name"
        let rest = &line["Device ".len()..];
        let (addr, name) = match rest.split_once(' ') {
            Some(t) => t,
            None => continue,
        };
        if name.starts_with(prefix) {
            out.push((addr.to_string(), name.to_string()));
        }
    }
    out
}

#[cfg(test)]
mod outbound_tests {
    use super::*;

    #[test]
    fn parse_bluetoothctl_filters_prefix() {
        let stdout = "\
Device 00:15:83:3D:0A:57 PIM-gatewaybtonly
Device AA:BB:CC:DD:EE:FF Random Speaker
Device 64:32:A8:14:4F:4B PIM-clientbtonly
";
        let v = parse_bluetoothctl_devices(stdout, "PIM-");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, "00:15:83:3D:0A:57");
        assert_eq!(v[0].1, "PIM-gatewaybtonly");
        assert_eq!(v[1].0, "64:32:A8:14:4F:4B");
        assert_eq!(v[1].1, "PIM-clientbtonly");
    }

    #[test]
    fn parse_bluetoothctl_handles_empty() {
        let v = parse_bluetoothctl_devices("", "PIM-");
        assert!(v.is_empty());
    }

    #[test]
    fn parse_bluetoothctl_skips_non_device_lines() {
        let stdout = "Controller AA:BB:CC:DD:EE:FF [default]\nDevice AA:BB:CC:DD:EE:FF PIM-x";
        let v = parse_bluetoothctl_devices(stdout, "PIM-");
        assert_eq!(v.len(), 1);
    }
}
