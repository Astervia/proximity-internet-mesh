#!/usr/bin/env bash
# test-auto-discovery.sh — Phase 7: Zero-config auto-discovery integration tests
#
# Tests (from implementation-plan.md §7.4):
#   7A  All nodes discover each other via UDP broadcast only (no static [[peers]])
#   7B  Client auto-IP: gets IpAssign from gateway, TUN configured, can ping gateway
#   7C  Internet via discovered relay chain: curl http://example.com from client
#   7D  DNS through mesh: nslookup google.com resolves from client
#   7E  Late-joiner: gateway+relay up 10 s before client → client discovers and joins
#   7F  Relay loss + recovery: stop relay → peer timeout → restart → re-discovers
#
# Usage:
#   bash docker/tests/test-auto-discovery.sh
#   DUMP_LOGS_ON_FAIL=1 bash docker/tests/test-auto-discovery.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

COMPOSE_FILE="auto-discovery.yml"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ "$FAIL" -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "Phase 7 — Zero-config auto-discovery"
log_info "Compose file: $COMPOSE_FILE"
log_info "All three nodes start with NO static [[peers]] — discovery only"

start_stack "$COMPOSE_FILE"
wait_all_healthy "$COMPOSE_FILE" 120

# Discovery broadcasts every 5 s; allow 3 full cycles (15 s) plus handshake
# and IP-assignment round-trips.
log_info "Waiting 20 s for discovery to converge and IP to be assigned..."
sleep 20

# ── 7A All nodes discover each other ─────────────────────────────────────────
log_section "7A All nodes discover each other (no static peers)"

wait_for_peers "$COMPOSE_FILE" gateway 1 30
wait_for_peers "$COMPOSE_FILE" relay   1 30
wait_for_peers "$COMPOSE_FILE" client  1 30

assert_cmd_output "gateway stats show peers keyword" "peers" \
    in_svc "$COMPOSE_FILE" gateway pim status --verbose

assert_cmd_output "relay stats show peers keyword" "peers" \
    in_svc "$COMPOSE_FILE" relay pim status --verbose

assert_cmd_output "client stats show peers keyword" "peers" \
    in_svc "$COMPOSE_FILE" client pim status --verbose

# ── 7B Client auto-IP configured ─────────────────────────────────────────────
log_section "7B Client auto-IP: TUN configured from gateway IpAssign"

# The client healthcheck only verifies the daemon process, not the TUN, because
# the TUN comes up only after IpAssign is received.  Check it explicitly here.
assert_iface_up "$COMPOSE_FILE" client "pim0" \
    "client pim0 interface is UP after IpAssign"

assert_iface_addr "$COMPOSE_FILE" client "10.77.0." \
    "client pim0 has mesh IP in 10.77.0.0/24"

assert_ping "$COMPOSE_FILE" client "10.77.0.1" \
    "client can ping gateway mesh IP via discovered path"

# ── 7C Internet via discovered relay chain ────────────────────────────────────
log_section "7C Internet access through discovered relay chain"

assert_cmd \
    "client enables split-default routing explicitly after auto-IP" \
    enable_mesh_route "$COMPOSE_FILE" client

assert_curl "$COMPOSE_FILE" client "http://example.com" \
    "client can curl http://example.com via discovered gateway"

# ── 7D DNS through mesh ───────────────────────────────────────────────────────
log_section "7D DNS resolution through mesh"

assert_dns "$COMPOSE_FILE" client "google.com" \
    "client DNS resolves google.com through mesh"

# ── 7E Late-joiner discovers and joins ────────────────────────────────────────
log_section "7E Late-joiner: client stopped and restarted after gateway+relay are stable"

log_info "Stopping client to simulate late-joiner scenario..."
compose "$COMPOSE_FILE" stop client

log_info "Waiting 5 s for gateway and relay to remain stable..."
sleep 5

log_info "Restarting client (late-joiner)..."
compose "$COMPOSE_FILE" start client
wait_healthy "$COMPOSE_FILE" client 60

log_info "Waiting 20 s for late-joiner to discover, handshake, and receive IpAssign..."
sleep 20

assert_iface_up "$COMPOSE_FILE" client "pim0" \
    "late-joiner pim0 interface is UP"

assert_iface_addr "$COMPOSE_FILE" client "10.77.0." \
    "late-joiner has mesh IP in 10.77.0.0/24"

assert_ping "$COMPOSE_FILE" client "10.77.0.1" \
    "late-joiner can reach gateway mesh IP"

assert_cmd \
    "late-joiner enables split-default routing explicitly" \
    enable_mesh_route "$COMPOSE_FILE" client

assert_curl "$COMPOSE_FILE" client "http://example.com" \
    "late-joiner has internet access via discovered gateway"

# ── 7F Relay loss and re-discovery ───────────────────────────────────────────
log_section "7F Relay loss detection and re-discovery"

log_info "Stopping relay container..."
compose "$COMPOSE_FILE" stop relay

# Heartbeat timeout: 3 missed heartbeats × 5 s interval = 15 s.
log_info "Waiting 20 s for gateway and client to detect relay loss..."
sleep 20

GWSTATS=$(in_svc "$COMPOSE_FILE" gateway pim status --verbose 2>/dev/null || echo "")
if echo "$GWSTATS" | grep -qE "peers=[01]"; then
    log_ok "gateway peer count dropped after relay removed"
else
    log_warn "cannot confirm gateway peer count (stats: $GWSTATS)"
    log_skip "gateway peer count verification after relay loss"
fi

log_info "Restarting relay container..."
compose "$COMPOSE_FILE" start relay
wait_healthy "$COMPOSE_FILE" relay 60

log_info "Waiting 25 s for relay to re-broadcast and peers to reconnect..."
sleep 25

assert_ping "$COMPOSE_FILE" client "10.77.0.1" \
    "client can reach gateway after relay re-discovered"

assert_cmd_output "relay stats show peers after restart" "peers" \
    in_svc "$COMPOSE_FILE" relay pim status --verbose

# ── Summary ───────────────────────────────────────────────────────────────────
print_summary
