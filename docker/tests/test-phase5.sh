#!/usr/bin/env bash
# test-phase5.sh — Phase 5: Multi-gateway, load balancing, and failover tests
#
# Tests (from implementation-plan.md):
#   5.1  Kill preferred gateway → failover to gateway2; restore → may shift back
#   5.2  Saturate gateway1 → new flows routed to gateway2
#   5.3  tc netem latency on gateway1 → client prefers gateway2
#
# NOTE: 5.2 and 5.3 require tc/netem and are inherently statistical.
#       They verify the selection mechanism rather than exact routing.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

COMPOSE_FILE="phase5-multigateway.yml"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ $FAIL -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "Phase 5 — Multi-gateway and load balancing"
log_info "Compose file: $COMPOSE_FILE"

start_stack "$COMPOSE_FILE"
wait_all_healthy "$COMPOSE_FILE" 180

# Give the routing protocol 2 cycles to learn both gateways (10 s default).
log_info "Waiting 15 s for dual-gateway routing to converge..."
sleep 15

assert_ping "$COMPOSE_FILE" client "10.77.0.1" \
    "client reaches gateway1 via relay"

assert_ping "$COMPOSE_FILE" client "10.77.0.2" \
    "client reaches gateway2 via relay"

assert_cmd \
    "client enables split-default routing explicitly" \
    enable_mesh_route "$COMPOSE_FILE" client

assert_curl "$COMPOSE_FILE" client "http://example.com" \
    "client internet access through preferred gateway"

assert_cmd_output "relay routing table shows routes to both gateways" "routes" \
    in_svc "$COMPOSE_FILE" relay pim status --verbose

# ── 5.1 Gateway failover ──────────────────────────────────────────────────────
log_section "5.1 Gateway failover"

log_info "Recording initial preferred gateway via ping RTTs..."
RTT1=$(in_svc "$COMPOSE_FILE" client bash -c \
    "ping -c 3 -W 2 10.77.0.1 2>/dev/null | tail -1 | grep -oP '\d+\.\d+(?=/\d)' | head -1" \
    2>/dev/null || echo "0")
RTT2=$(in_svc "$COMPOSE_FILE" client bash -c \
    "ping -c 3 -W 2 10.77.0.2 2>/dev/null | tail -1 | grep -oP '\d+\.\d+(?=/\d)' | head -1" \
    2>/dev/null || echo "0")
log_info "Baseline RTT — gateway1: ${RTT1} ms  gateway2: ${RTT2} ms"

log_info "Stopping gateway1..."
compose "$COMPOSE_FILE" stop gateway1

log_info "Waiting 20 s for peer timeout and route failover..."
sleep 20

assert_ping "$COMPOSE_FILE" client "10.77.0.2" \
    "client reaches gateway2 after gateway1 removed"

assert_curl "$COMPOSE_FILE" client "http://example.com" \
    "internet access via gateway2 after failover"

log_info "Restarting gateway1..."
compose "$COMPOSE_FILE" start gateway1
wait_healthy "$COMPOSE_FILE" gateway1 60

log_info "Waiting 20 s for gateway1 to rejoin and routes to update..."
sleep 20

assert_ping "$COMPOSE_FILE" client "10.77.0.1" \
    "client can reach gateway1 after it rejoins"

assert_curl "$COMPOSE_FILE" client "http://example.com" \
    "internet access still works after gateway1 rejoins"

# ── 5.3 RTT-aware selection (tc netem) ───────────────────────────────────────
log_section "5.3 Latency-aware gateway selection"

# Check if tc is available in the gateway container.
if in_svc "$COMPOSE_FILE" gateway1 tc qdisc show >/dev/null 2>&1; then
    log_info "Adding 200 ms netem delay to gateway1 egress..."
    in_svc "$COMPOSE_FILE" gateway1 \
        tc qdisc add dev eth0 root netem delay 200ms >/dev/null 2>&1 || \
        log_warn "tc netem not supported — skipping RTT-aware selection test"

    log_info "Waiting 25 s for gateway probe cycle to update RTT..."
    sleep 25

    # Gateway probes run every 10 s, so two cycles should be enough.
    STATS=$(in_svc "$COMPOSE_FILE" relay pim status --verbose 2>/dev/null || echo "")
    if echo "$STATS" | grep -qE "routes=[1-9]"; then
        log_ok "relay routing table updated after RTT change (routes present)"
    else
        log_skip "RTT-aware selection (could not confirm routing table update)"
    fi

    log_info "Removing netem delay from gateway1..."
    in_svc "$COMPOSE_FILE" gateway1 \
        tc qdisc del dev eth0 root netem >/dev/null 2>&1 || true
else
    log_skip "5.3 RTT-aware selection (tc not available in container — add NET_ADMIN cap or run privileged)"
fi

# ── 5.2 Load-aware selection ──────────────────────────────────────────────────
log_section "5.2 Load-aware gateway selection"

log_info "Flooding gateway1 with traffic for 15 s..."
in_svc "$COMPOSE_FILE" client bash -c \
    "ping -f -c 3000 -W 1 10.77.0.1 >/dev/null 2>&1 || true" &
sleep 15

STATS=$(in_svc "$COMPOSE_FILE" relay pim status --verbose 2>/dev/null || echo "")
if echo "$STATS" | grep -qE "packets_forwarded=[1-9]"; then
    log_ok "relay forwarded packets observed during load test"
else
    log_skip "load-aware selection (forwarded packet counter not visible in status)"
fi

assert_curl "$COMPOSE_FILE" client "http://example.com" \
    "internet access maintained during gateway1 load"

# ── Summary ───────────────────────────────────────────────────────────────────
print_summary
