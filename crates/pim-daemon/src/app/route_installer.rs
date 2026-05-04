//! Background task that reconciles the kernel's split-default routes
//! against `state.route_on` + the routing table's currently-selected
//! gateway.
//!
//! Reactivity:
//!   - `state.route_install_notify.notified()` — woken from
//!     `route.set_split_default` so a UI toggle takes effect within
//!     one async hop instead of waiting up to 2 s for the tick.
//!   - 2 s reconciliation tick — picks up gateway-selection changes
//!     (multi-gateway swings, gateway disappearance) without an
//!     event channel; gateway moves are rare enough that polling is
//!     simpler than wiring per-event hooks through the routing engine.
//!   - `state.cancel.cancelled()` — runs the final removal so the
//!     daemon doesn't leave orphan `0.0.0.0/1`/`128.0.0.0/1` routes
//!     pointing at a soon-to-be-gone `pim0` after a clean shutdown.
//!
//! Convergence rules (`route_on`, `selected_gateway`) → action:
//!   - `(false, _)`                       → remove any installed route.
//!   - `(true,  None)`                    → remove (kill-switch shape:
//!     UI is asking us to route through the mesh but no gateway is
//!     reachable, so dropping the routes is safer than letting traffic
//!     fall back through wifi behind the user's back).
//!   - `(true,  Some(ip))`, ip == current → no-op.
//!   - `(true,  Some(ip))`, ip != current → atomic replace (remove old
//!     via, install new via).
//!
//! Gateway nodes (`state.is_gateway`) skip the installer entirely —
//! the gateway IS the destination, so installing a split-default route
//! through itself would be a loop.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::app::DaemonState;

/// Public DNS resolvers handed to systemd-resolved when split-default
/// routing is engaged. Picked because they're (a) anycast — reachable
/// over any internet uplink the gateway might have, (b) fast on the
/// global mean, (c) reasonably privacy-conscious. Listing two so a
/// single resolver outage doesn't break the mesh.
const MESH_DNS_SERVERS: &[&str] = &["1.1.1.1", "1.0.0.1", "8.8.8.8"];

/// IPv6 split-default routes are `dev pim0`-only — `pim-tun` ignores
/// the `gateway_ipv6` parameter on Linux/macOS (see
/// `pim-tun::TunInterface::add_default_ipv6_route`). All we track is
/// "are the v6 routes installed", and we use the unspecified `::1` as
/// the placeholder argument since the API requires *something*.
const PLACEHOLDER_V6_GATEWAY: Ipv6Addr = Ipv6Addr::LOCALHOST;

/// Reconciliation tick. Bounded so a missed `notify` (concurrent
/// notify-then-await race) still converges within 2 s, and so
/// gateway-selection changes that don't go through the RPC handler
/// (load / RTT swings, peer-loss-triggered route invalidation) get
/// followed without extra plumbing.
const RECONCILE_PERIOD: Duration = Duration::from_secs(2);

/// Spawn the route installer. Returns immediately; the task lives
/// until `state.cancel` fires.
pub(crate) fn spawn(state: Arc<DaemonState>) {
    if state.is_gateway {
        debug!("route installer: skipped (this node is itself a gateway)");
        return;
    }
    tokio::spawn(run(state));
}

async fn run(state: Arc<DaemonState>) {
    info!("route installer: started");
    let mut current_via_v4: Option<Ipv4Addr> = None;
    let mut current_v6: bool = false;
    // Tracks whether we've configured pim0's DNS via systemd-resolved.
    // Coupled to V4 install state — if either the V4 or V6 routes are
    // up we want apps to resolve names through the mesh; if both are
    // down we want the default resolver back. We key on `current_via_v4`
    // OR `current_v6` below.
    let mut current_dns: bool = false;
    let iface_name = state.tun.name().to_string();

    loop {
        tokio::select! {
            _ = state.route_install_notify.notified() => {
                debug!("route installer: woken by notify");
            }
            _ = tokio::time::sleep(RECONCILE_PERIOD) => {}
            _ = state.cancel.cancelled() => {
                if let Some(old) = current_via_v4.take() {
                    if let Err(e) = state.tun.remove_default_route(old) {
                        warn!(via = %old, "route installer shutdown: V4 remove failed: {e}");
                    } else {
                        info!(via = %old, "route installer shutdown: V4 split-default routes removed");
                    }
                }
                if current_v6 {
                    if let Err(e) = state.tun.remove_default_ipv6_route(PLACEHOLDER_V6_GATEWAY) {
                        warn!("route installer shutdown: V6 remove failed: {e}");
                    } else {
                        info!("route installer shutdown: V6 split-default routes removed");
                    }
                }
                if current_dns {
                    revert_interface_dns(&iface_name);
                }
                info!("route installer: stopped");
                return;
            }
        }

        let route_on = state.route_on.load(Ordering::SeqCst);

        // ── V4: route via the SELECTED gateway's mesh IP ─────────────
        // `nearest_gateway_mesh_ip` follows multi-gateway swings without
        // a dedicated event channel — gateway_score may pick a different
        // gateway any tick (load / RTT / hop count drift), and the
        // installer's atomic-replace below makes that a clean swap.
        let desired_via_v4: Option<Ipv4Addr> = if route_on {
            let routing = state.routing.lock().await;
            routing.nearest_gateway_mesh_ip()
        } else {
            None
        };

        if desired_via_v4 != current_via_v4 {
            if let Some(old) = current_via_v4.take() {
                if let Err(e) = state.tun.remove_default_route(old) {
                    warn!(via = %old, "remove old V4 split-default route failed: {e}");
                } else {
                    info!(via = %old, "V4 split-default routes removed");
                }
            }
            if let Some(new) = desired_via_v4 {
                match state.tun.add_default_route(new) {
                    Ok(()) => {
                        info!(via = %new, "V4 split-default routes installed");
                        current_via_v4 = Some(new);
                    }
                    Err(e) => {
                        warn!(via = %new, "install V4 split-default route failed: {e}");
                        // Leave current_via_v4=None so the next tick retries.
                    }
                }
            }
        }

        // ── V6: dev-only routes; install whenever route_on AND we have a
        //       configured mesh IPv6 (the per-gateway mesh_ipv6 isn't
        //       yet exposed by the routing table, so we don't switch on
        //       gateway-selection — the v6 path is single-gateway today).
        let desired_v6 = route_on && state.mesh_ipv6.read().await.is_some();
        if desired_v6 != current_v6 {
            if desired_v6 {
                match state.tun.add_default_ipv6_route(PLACEHOLDER_V6_GATEWAY) {
                    Ok(()) => {
                        info!("V6 split-default routes installed");
                        current_v6 = true;
                    }
                    Err(e) => {
                        warn!("install V6 split-default route failed: {e}");
                        // Leave current_v6=false so the next tick retries.
                    }
                }
            } else {
                if let Err(e) = state.tun.remove_default_ipv6_route(PLACEHOLDER_V6_GATEWAY) {
                    warn!("remove V6 split-default route failed: {e}");
                } else {
                    info!("V6 split-default routes removed");
                }
                current_v6 = false;
            }
        }

        // ── DNS: hand systemd-resolved a public resolver pinned to
        //        pim0 whenever any mesh route is up; revert when both
        //        come down. Without this, when the user disables wifi
        //        the system's DHCP-derived nameserver becomes
        //        unreachable and apps report "no internet" even though
        //        the IP path through the mesh is live (curl by IP works,
        //        curl by hostname doesn't). Anycast resolvers
        //        (1.1.1.1, 8.8.8.8) flow through the mesh + gateway NAT
        //        like any other internet destination.
        let desired_dns = current_via_v4.is_some() || current_v6;
        if desired_dns != current_dns {
            if desired_dns {
                if set_interface_dns(&iface_name, MESH_DNS_SERVERS) {
                    current_dns = true;
                }
            } else {
                revert_interface_dns(&iface_name);
                current_dns = false;
            }
        }
    }
}

/// Configure systemd-resolved to use `servers` as the DNS upstream for
/// queries arriving on `iface`, with a wildcard search domain so it
/// becomes the *default* resolver while routing is engaged. Returns
/// `true` on success so the caller can flip its `current_dns` flag;
/// returns `false` (with a warn log) if `resolvectl` isn't installed
/// or fails — the routes are still useful for IP-only traffic and the
/// user can resolve manually with `--dns-servers`.
///
/// Linux-only because systemd-resolved is the only major resolver that
/// exposes a per-interface DNS API. macOS uses scutil/scsetup which
/// has different ergonomics; Windows uses netsh. Neither runs the
/// daemon today (the route_installer itself is Linux-pim0-only), so
/// the gating below is mostly defensive.
#[cfg(target_os = "linux")]
fn set_interface_dns(iface: &str, servers: &[&str]) -> bool {
    let mut dns_args: Vec<&str> = vec!["dns", iface];
    dns_args.extend_from_slice(servers);
    let dns_status = std::process::Command::new("resolvectl")
        .args(&dns_args)
        .status();
    match dns_status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            warn!(
                iface,
                exit = ?s.code(),
                "resolvectl dns failed; DNS via mesh will not work until set manually"
            );
            return false;
        }
        Err(e) => {
            warn!(
                iface,
                "resolvectl unavailable ({e}); DNS via mesh will not work until set manually \
                 (e.g. `resolvectl dns {iface} 1.1.1.1` and `resolvectl domain {iface} '~.'`)"
            );
            return false;
        }
    }
    // The wildcard search domain `~.` makes pim0's resolvers the global
    // fallback for every query, not just for names ending in a
    // pim-specific suffix. Without this, only mDNS-style local domains
    // would route through pim0 and global names (gmail.com, etc.) would
    // still hit the now-broken DHCP resolver.
    let dom_status = std::process::Command::new("resolvectl")
        .args(["domain", iface, "~."])
        .status();
    match dom_status {
        Ok(s) if s.success() => {
            info!(iface, servers = ?servers, "configured pim0 as default DNS resolver");
            true
        }
        Ok(s) => {
            warn!(
                iface,
                exit = ?s.code(),
                "resolvectl domain '~.' failed; pim0 DNS set but not promoted to default \
                 — global hostnames may still hit the wifi resolver"
            );
            // The dns assignment did succeed; treat as half-success so
            // we still revert it on teardown.
            true
        }
        Err(e) => {
            warn!(iface, "resolvectl domain unavailable ({e})");
            true
        }
    }
}

/// Revert any per-interface DNS configuration we set on `iface`.
/// Idempotent + tolerant: if `resolvectl` is missing or the interface
/// has no overrides, the call no-ops and we log at debug.
#[cfg(target_os = "linux")]
fn revert_interface_dns(iface: &str) {
    match std::process::Command::new("resolvectl")
        .args(["revert", iface])
        .status()
    {
        Ok(s) if s.success() => {
            info!(iface, "reverted pim0 DNS overrides");
        }
        Ok(s) => {
            debug!(
                iface,
                exit = ?s.code(),
                "resolvectl revert exited non-zero (probably no overrides to revert)"
            );
        }
        Err(e) => {
            debug!(iface, "resolvectl revert failed ({e})");
        }
    }
}

// On non-Linux targets the route installer never spawns (route_installer
// is Linux-only by virtue of pim-tun's add_default_route being a no-op
// elsewhere), but keep stub signatures so the rest of the file builds
// on macOS / Windows daemon configurations.
#[cfg(not(target_os = "linux"))]
fn set_interface_dns(_iface: &str, _servers: &[&str]) -> bool {
    false
}

#[cfg(not(target_os = "linux"))]
fn revert_interface_dns(_iface: &str) {}
