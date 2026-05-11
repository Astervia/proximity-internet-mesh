//! Desktop-initiated BR/EDR inquiry + auto-pair loop.
//!
//! This is the desktop-side counterpart to the Android shell's
//! `BluetoothPlugin.startDiscovery` (`ui/src-tauri/gen/android/.../
//! BluetoothPlugin.kt`). Together they let either side bootstrap a
//! PIM bond without the user having to open OS Bluetooth Settings
//! and pair by hand.
//!
//! The loop is intentionally simple — it shells out to `bluetoothctl`
//! rather than speaking BlueZ's D-Bus directly:
//!
//! 1. `bluetoothctl --timeout=12 scan on` triggers a ~12 s BR/EDR
//!    inquiry. The command exits when the timer expires, but BlueZ
//!    keeps the discovered devices in its cache.
//! 2. `bluetoothctl devices` lists every device BlueZ knows about,
//!    including non-paired ones from the inquiry above. Filter by
//!    name prefix (`PIM-` by default).
//! 3. For each candidate that isn't already `Paired: yes` in
//!    `bluetoothctl info <addr>`, run `bluetoothctl pair <addr>`
//!    then `bluetoothctl trust <addr>`. BlueZ's default agent pops
//!    the PIN-confirm dialog on both sides via Polkit / the OS
//!    desktop environment's notification daemon; the user accepts.
//! 4. Per-address cooldown after a declined / failed pair (default
//!    10 min, matching the Android side and the macOS sidecar's
//!    `COOLDOWN_DECLINED_PAIRING_SECS`).
//!
//! On a successful pair, the existing `outbound` paired-device scan
//! picks up the new bond on its next poll, dials RFCOMM, and emits
//! `RfcommEvent::Discovered`. So this module's job is *only* to add
//! the bond — it does not dial directly.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{RfcommConfig, RfcommEvent};

/// How long to wait after a declined / failed pair before re-attempting
/// the same address. Matches `BluetoothPlugin.kt` Android side and the
/// macOS sidecar's value so user-facing behaviour is consistent.
const DECLINE_COOLDOWN: Duration = Duration::from_secs(600);

/// `bluetoothctl scan on --timeout=<secs>` runs inquiry for this many
/// seconds per cycle. 12 s matches the BR/EDR Page-Scan-Window default
/// in most controllers — longer is wasted, shorter sometimes misses
/// devices that are advertising on a slow schedule.
const SCAN_DURATION_S: u64 = 12;

/// Spawn the inquiry + pair task. Cancels cleanly on the cancel token;
/// errors become `RfcommEvent::Error` so the daemon's log includes
/// them without aborting the whole rfcomm service.
pub fn spawn(cfg: RfcommConfig, events_tx: mpsc::Sender<RfcommEvent>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut cooldowns: HashMap<String, Instant> = HashMap::new();
        // Run one inquiry cycle right away so a fresh daemon doesn't
        // wait `inquiry_interval` before its first scan; otherwise the
        // user-visible delay between starting the daemon and pairing
        // the first phone is up to 60 s.
        run_one_cycle(&cfg, &events_tx, &mut cooldowns).await;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!(target: "pim-bluetooth-rfcomm", "inquiry shutdown");
                    return;
                }
                _ = sleep(cfg.inquiry_interval) => {}
            }
            run_one_cycle(&cfg, &events_tx, &mut cooldowns).await;
        }
    });
}

async fn run_one_cycle(
    cfg: &RfcommConfig,
    events_tx: &mpsc::Sender<RfcommEvent>,
    cooldowns: &mut HashMap<String, Instant>,
) {
    if let Err(e) = run_scan(&cfg.bluetoothctl_command).await {
        warn!(target: "pim-bluetooth-rfcomm", "inquiry scan failed: {e}");
        let _ = events_tx
            .send(RfcommEvent::Error {
                code: -33010,
                message: format!("inquiry scan failed: {e}"),
            })
            .await;
        return;
    }
    let candidates = match list_discovered_pim_devices(&cfg.bluetoothctl_command, &cfg.prefix).await
    {
        Ok(v) => v,
        Err(e) => {
            warn!(target: "pim-bluetooth-rfcomm", "list discovered failed: {e}");
            return;
        }
    };
    info!(
        target: "pim-bluetooth-rfcomm",
        discovered = candidates.len(),
        "rfcomm inquiry cycle"
    );

    let now = Instant::now();
    cooldowns.retain(|_, until| *until > now);

    for (bd_addr_str, name) in candidates {
        if let Some(until) = cooldowns.get(&bd_addr_str) {
            if *until > now {
                let remaining = until.saturating_duration_since(now).as_secs();
                debug!(
                    target: "pim-bluetooth-rfcomm",
                    bd_addr = %bd_addr_str,
                    cooldown_remaining_s = remaining,
                    "skipping cooldown'd peer"
                );
                continue;
            }
        }
        match is_paired(&cfg.bluetoothctl_command, &bd_addr_str).await {
            Ok(true) => continue, // outbound dial loop will handle it
            Ok(false) => {}
            Err(e) => {
                warn!(
                    target: "pim-bluetooth-rfcomm",
                    bd_addr = %bd_addr_str,
                    "info check failed: {e}"
                );
                continue;
            }
        }
        info!(
            target: "pim-bluetooth-rfcomm",
            bd_addr = %bd_addr_str,
            name = %name,
            "rfcomm inquiry: attempting pair"
        );
        match run_pair_trust(&cfg.bluetoothctl_command, &bd_addr_str).await {
            Ok(()) => info!(
                target: "pim-bluetooth-rfcomm",
                bd_addr = %bd_addr_str,
                "rfcomm inquiry: paired + trusted"
            ),
            Err(e) => {
                warn!(
                    target: "pim-bluetooth-rfcomm",
                    bd_addr = %bd_addr_str,
                    "pair failed: {e}; cooling down for {}s",
                    DECLINE_COOLDOWN.as_secs()
                );
                cooldowns.insert(bd_addr_str.clone(), Instant::now() + DECLINE_COOLDOWN);
            }
        }
    }
}

async fn run_scan(bluetoothctl_command: &Path) -> std::io::Result<()> {
    // `--timeout` bounds the inquiry so `scan on` returns instead of
    // sitting on an interactive prompt. The output is noisy (one
    // `[NEW]` line per discovered device + agent prompts); we don't
    // parse it directly — the followup `bluetoothctl devices` call
    // pulls the cached set from BlueZ.
    let timeout_arg = format!("--timeout={SCAN_DURATION_S}");
    let out = Command::new(bluetoothctl_command)
        .args([timeout_arg.as_str(), "scan", "on"])
        .output()
        .await?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "bluetoothctl scan on exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

async fn list_discovered_pim_devices(
    bluetoothctl_command: &Path,
    prefix: &str,
) -> std::io::Result<Vec<(String, String)>> {
    let out = Command::new(bluetoothctl_command)
        .args(["devices"])
        .output()
        .await?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "bluetoothctl devices exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(parse_devices(&stdout, prefix))
}

async fn is_paired(bluetoothctl_command: &Path, bd_addr: &str) -> std::io::Result<bool> {
    let out = Command::new(bluetoothctl_command)
        .args(["info", bd_addr])
        .output()
        .await?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "bluetoothctl info {bd_addr} exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(parse_paired_yes(&stdout))
}

async fn run_pair_trust(bluetoothctl_command: &Path, bd_addr: &str) -> std::io::Result<()> {
    let pair = Command::new(bluetoothctl_command)
        .args(["pair", bd_addr])
        .output()
        .await?;
    if !pair.status.success() {
        return Err(std::io::Error::other(format!(
            "bluetoothctl pair {bd_addr} exit {}: {}",
            pair.status,
            String::from_utf8_lossy(&pair.stderr).trim()
        )));
    }
    // Trust skips the agent's authorisation prompt on subsequent
    // connections; without it BlueZ would re-prompt every reconnect.
    let trust = Command::new(bluetoothctl_command)
        .args(["trust", bd_addr])
        .output()
        .await?;
    if !trust.status.success() {
        return Err(std::io::Error::other(format!(
            "bluetoothctl trust {bd_addr} exit {}: {}",
            trust.status,
            String::from_utf8_lossy(&trust.stderr).trim()
        )));
    }
    Ok(())
}

/// Parse `bluetoothctl devices` output for `Device <addr> <name>`
/// lines whose name starts with `prefix`. The `devices` (no
/// subcommand) call returns BlueZ's full known-devices set —
/// `Paired`, `Trusted`, `Bonded` etc. plus inquiry-discovered
/// non-paired peers from the most recent scan. We filter
/// already-paired entries higher up in [`is_paired`].
pub(crate) fn parse_devices(stdout: &str, prefix: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if !line.starts_with("Device ") {
            continue;
        }
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

/// Pull the `Paired: yes/no` line out of `bluetoothctl info` output.
/// Missing line = not paired (BlueZ omits the property until the
/// stack has a bonding record).
pub(crate) fn parse_paired_yes(stdout: &str) -> bool {
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Paired:") {
            return rest.trim() == "yes";
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_devices_filters_prefix() {
        let stdout = "\
Device 00:15:83:3D:0A:57 PIM-gatewaybtonly
Device AA:BB:CC:DD:EE:FF Random Speaker
Device 64:32:A8:14:4F:4B PIM-clientbtonly
";
        let v = parse_devices(stdout, "PIM-");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, "00:15:83:3D:0A:57");
        assert_eq!(v[1].0, "64:32:A8:14:4F:4B");
    }

    #[test]
    fn parse_paired_yes_handles_typical_info() {
        let stdout = "\
Device AA:BB:CC:DD:EE:FF (public)
\tName: PIM-foo
\tAlias: PIM-foo
\tPaired: yes
\tTrusted: yes
";
        assert!(parse_paired_yes(stdout));
    }

    #[test]
    fn parse_paired_no() {
        let stdout = "\
Device AA:BB:CC:DD:EE:FF (public)
\tName: PIM-foo
\tPaired: no
";
        assert!(!parse_paired_yes(stdout));
    }

    #[test]
    fn parse_paired_missing_field_is_false() {
        assert!(!parse_paired_yes(
            "Device AA:BB:CC:DD:EE:FF\n\tName: PIM-foo\n"
        ));
    }
}
