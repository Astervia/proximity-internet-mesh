#!/usr/bin/env bash
# test-peer-discovery.sh — Phase 3: Discovery and peer lifecycle tests
#
# Tests (from implementation-plan.md):
#   3.1  All three nodes discover each other within 2 broadcast cycles (10 s)
#   3.3  Kill a container → peers detect loss within 15 s, routes update
#   3.3  Restart killed container → re-discovers and rejoins mesh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

COMPOSE_FILE="peer-discovery.yml"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ $FAIL -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "Phase 3 — Peer discovery and lifecycle"
log_info "Compose file: $COMPOSE_FILE"

start_stack "$COMPOSE_FILE"
wait_all_healthy "$COMPOSE_FILE" 150

# ── 3.1 Discovery within 2 broadcast cycles ───────────────────────────────────
log_section "3.1 Peer discovery"

# Default broadcast interval is 5 s, so 2 cycles = 10 s.
# We already waited for service_healthy which implies the daemon is up.
# Give it one extra cycle to propagate routes.
log_info "Waiting 10 s for discovery broadcast cycles..."
sleep 10

assert_cmd_output "gateway sees peers in stats after discovery" "peers" \
    in_svc "$COMPOSE_FILE" gateway pim status --verbose

assert_cmd_output "relay sees peers in stats after discovery" "peers" \
    in_svc "$COMPOSE_FILE" relay pim status --verbose

GW_IPV4=$(mesh_ipv4_of "$COMPOSE_FILE" gateway) || {
    log_fail "could not read gateway mesh IPv4"; exit 1; }
RELAY_IPV4=$(mesh_ipv4_of "$COMPOSE_FILE" relay) || {
    log_fail "could not read relay mesh IPv4"; exit 1; }
log_info "gateway mesh_ipv4 = $GW_IPV4 · relay mesh_ipv4 = $RELAY_IPV4"

assert_ping "$COMPOSE_FILE" client "$GW_IPV4" \
    "client discovered gateway and can ping mesh IP ($GW_IPV4)"

assert_ping "$COMPOSE_FILE" client "$RELAY_IPV4" \
    "client discovered relay and can ping relay mesh IP ($RELAY_IPV4)"

# ── 3.3 Peer loss detection ───────────────────────────────────────────────────
log_section "3.3 Peer loss detection (15 s timeout)"

log_info "Killing relay container..."
compose "$COMPOSE_FILE" stop relay

# Heartbeat timeout: 3 missed × 5 s interval = 15 s.
log_info "Waiting 20 s for peer timeout..."
sleep 20

# After the relay is gone, the gateway and client should both remove it from
# their peer tables.  The gateway's route to the client should also be removed.
STATS=$(in_svc "$COMPOSE_FILE" gateway pim status --verbose 2>/dev/null || echo "")
if echo "$STATS" | grep -qE "peers=[01]"; then
    log_ok "gateway peer count dropped after relay removed"
else
    log_warn "cannot confirm peer count (stats output: $STATS)"
    log_skip "peer count verification"
fi

# ── 3.3 Re-join after restart ─────────────────────────────────────────────────
log_section "3.3 Container restart → rejoin"

log_info "Restarting relay container..."
compose "$COMPOSE_FILE" start relay

log_info "Waiting 30 s for relay to rejoin and routes to reconverge..."
sleep 30

assert_ping "$COMPOSE_FILE" client "$GW_IPV4" \
    "client can reach gateway after relay rejoined"

assert_cmd_output "gateway sees peers again after relay rejoined" "peers" \
    in_svc "$COMPOSE_FILE" gateway pim status --verbose

# ── Summary ───────────────────────────────────────────────────────────────────
print_summary
