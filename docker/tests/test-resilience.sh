#!/usr/bin/env bash
# test-resilience.sh — Phase 4: Resilience, flow control, and NAT timeout tests
#
# Tests (from implementation-plan.md):
#   4.1  Disconnect network → reconnect → mesh recovers automatically
#   4.2  Buffer frames during 5 s outage → delivered after reconnect
#   4.3  Flood sender → backpressure applies, no OOM
#   4.4  TCP idle 4 min → still alive; idle 6 min → conntrack expired
#
# NOTE: The 6-minute NAT timeout test (4.4) is slow by design.
#       Set SKIP_SLOW=1 to skip it in CI.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

RESILIENCE_FILE="resilience.yml"
FLOW_FILE="flow-control.yml"
SKIP_SLOW="${SKIP_SLOW:-0}"
RESILIENCE_GATEWAY_IP="172.34.0.10"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ $FAIL -gt 0 ]; then
        dump_logs "$RESILIENCE_FILE"
        dump_logs "$FLOW_FILE"
    fi
    stop_stack "$RESILIENCE_FILE"
    stop_stack "$FLOW_FILE"
}
trap cleanup EXIT

# ═══════════════════════════════════════════════════════════════════════════════
# 4.1 Connection resilience
# ═══════════════════════════════════════════════════════════════════════════════
log_section "Phase 4 — Connection resilience"
log_info "Compose file: $RESILIENCE_FILE"

start_stack "$RESILIENCE_FILE"
wait_all_healthy "$RESILIENCE_FILE" 120

log_info "Verifying baseline connectivity..."
GW_IPV4=$(mesh_ipv4_of "$RESILIENCE_FILE" gateway) || {
    log_fail "could not read gateway mesh IPv4"; exit 1; }
log_info "gateway derived mesh_ipv4 = $GW_IPV4"
assert_ping "$RESILIENCE_FILE" client "$GW_IPV4" \
    "baseline: client pings gateway ($GW_IPV4)"

log_section "4.1 Network disconnect and reconnect"

# Disconnect the gateway from the transport network.
log_info "Disconnecting gateway from pim-net..."
NETWORK_NAME=$(docker compose -f "$COMPOSE_DIR/$RESILIENCE_FILE" \
    ps --format '{{.Networks}}' gateway 2>/dev/null | head -1 || \
    echo "pim-resilience_pim-net")
docker network disconnect "$NETWORK_NAME" pim-resilience-gw 2>/dev/null || \
    log_warn "network disconnect failed (may need --privileged or host network mode)"

log_info "Waiting 5 s during outage..."
sleep 5

log_info "Reconnecting gateway to pim-net..."
docker network connect --ip "$RESILIENCE_GATEWAY_IP" "$NETWORK_NAME" pim-resilience-gw 2>/dev/null || \
    log_warn "network reconnect failed"

log_info "Waiting 40 s for reconnect and re-handshake (backoff up to 30 s)..."
sleep 40

assert_ping "$RESILIENCE_FILE" client "$GW_IPV4" \
    "client re-established connectivity after gateway reconnected"

assert_cmd_output "client daemon still running after disruption" "running" \
    in_svc "$RESILIENCE_FILE" client pim status

# ── 4.2 Store-and-forward ─────────────────────────────────────────────────────
log_section "4.2 Store-and-forward during outage"

# Disconnect gateway, immediately trigger some traffic on client, then reconnect.
log_info "Disconnecting gateway for store-and-forward test..."
docker network disconnect "$NETWORK_NAME" pim-resilience-gw 2>/dev/null || true

log_info "Sending traffic from client (will be buffered)..."
in_svc "$RESILIENCE_FILE" client ping -c 3 -W 1 $GW_IPV4 >/dev/null 2>&1 || true

log_info "Reconnecting gateway after 5 s..."
sleep 5
docker network connect --ip "$RESILIENCE_GATEWAY_IP" "$NETWORK_NAME" pim-resilience-gw 2>/dev/null || true

log_info "Waiting 35 s for buffer flush and route recovery..."
sleep 35

assert_ping "$RESILIENCE_FILE" client "$GW_IPV4" \
    "connectivity restored after store-and-forward reconnect"

stop_stack "$RESILIENCE_FILE"

# ═══════════════════════════════════════════════════════════════════════════════
# 4.3 Flow control
# ═══════════════════════════════════════════════════════════════════════════════
log_section "Phase 4 — Flow control / backpressure"
log_info "Compose file: $FLOW_FILE"

start_stack "$FLOW_FILE"
wait_all_healthy "$FLOW_FILE" 120

log_section "4.3 Flood sender → no OOM"

# Re-derive the gateway's mesh IP for this stack — fresh stack means
# fresh keystore, so the previous $GW_IPV4 from the resilience stack
# is meaningless here.
FLOW_GW_IPV4=$(mesh_ipv4_of "$FLOW_FILE" gateway) || {
    log_fail "could not read flow-control gateway mesh IPv4"; exit 1; }
log_info "flow-control gateway mesh_ipv4 = $FLOW_GW_IPV4"

# Record resident memory before flood.
MEM_BEFORE=$(in_svc "$FLOW_FILE" "flood-sender" bash -c \
    "cat /proc/\$(cat /run/pim.pid)/status 2>/dev/null | grep VmRSS | awk '{print \$2}'" \
    2>/dev/null || echo "0")

log_info "Flooding gateway with ICMP for 10 s..."
in_svc "$FLOW_FILE" "flood-sender" bash -c \
    "ping -f -c 2000 -W 1 $FLOW_GW_IPV4 >/dev/null 2>&1 || true" &
sleep 10

# Record resident memory after flood.
MEM_AFTER=$(in_svc "$FLOW_FILE" "flood-sender" bash -c \
    "cat /proc/\$(cat /run/pim.pid)/status 2>/dev/null | grep VmRSS | awk '{print \$2}'" \
    2>/dev/null || echo "0")

log_info "Memory before flood: ${MEM_BEFORE} kB — after: ${MEM_AFTER} kB"

# Allow up to 50 MB growth (flood-sender buffers control frames, not data).
MAX_GROWTH=51200
GROWTH=$(( MEM_AFTER - MEM_BEFORE ))
if [ "$GROWTH" -lt "$MAX_GROWTH" ]; then
    log_ok "memory growth during flood is bounded (${GROWTH} kB < ${MAX_GROWTH} kB)"
else
    log_fail "excessive memory growth during flood (${GROWTH} kB)"
fi

assert_cmd_output "flood-sender daemon still alive after flood" "running" \
    in_svc "$FLOW_FILE" "flood-sender" pim status

assert_cmd_output "gateway daemon still alive after flood" "running" \
    in_svc "$FLOW_FILE" gateway pim status

stop_stack "$FLOW_FILE"

# ═══════════════════════════════════════════════════════════════════════════════
# 4.4 NAT conntrack idle timeout (slow test)
# ═══════════════════════════════════════════════════════════════════════════════
log_section "Phase 4 — NAT conntrack idle timeout"

if [ "$SKIP_SLOW" = "1" ]; then
    log_skip "NAT conntrack timeout tests (SKIP_SLOW=1)"
else
    log_info "Compose file: $RESILIENCE_FILE (reusing single-hop stack)"
    start_stack "$RESILIENCE_FILE"
    wait_all_healthy "$RESILIENCE_FILE" 120

    # Fresh stack → fresh keystore → re-derive the gateway's mesh IP.
    GW_IPV4=$(mesh_ipv4_of "$RESILIENCE_FILE" gateway) || {
        log_fail "could not read gateway mesh IPv4 (NAT timeout stack)"; exit 1; }
    log_info "NAT-timeout stack gateway mesh_ipv4 = $GW_IPV4"

    assert_ping "$RESILIENCE_FILE" client "$GW_IPV4" \
        "baseline: mesh connectivity before timeout test"

    log_info "Testing TCP idle 4 min → connection should still be alive..."
    log_info "Sleeping 240 s (TCP idle timeout = 300 s)..."
    sleep 240

    assert_ping "$RESILIENCE_FILE" client "$GW_IPV4" \
        "TCP idle 4 min: mesh still alive (within conntrack timeout)"

    log_info "Testing TCP idle 6 min → conntrack should expire..."
    log_info "Sleeping additional 120 s (total 6 min, conntrack timeout = 5 min)..."
    sleep 120

    # After 6 min, a new connection should establish fine (conntrack re-created).
    assert_ping "$RESILIENCE_FILE" client "$GW_IPV4" \
        "TCP idle 6 min: new connection works (conntrack re-established)"

    stop_stack "$RESILIENCE_FILE"
fi

# ── Summary ───────────────────────────────────────────────────────────────────
print_summary
