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
    }
}
