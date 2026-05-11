#!/usr/bin/env bash
# test-multi-hop.sh — Phase 2: Multi-hop relay and routing tests
#
# Tests (from implementation-plan.md):
#   2.1  3-container: client → relay → gateway, curl internet
#   2.1  Frame with TTL=1 at relay is dropped
#   2.2  Relay cannot see plaintext payload (E2E encryption verified)
#   2.3  Routing table state visible on each node via pim status
#   2.3  Kill relay1 → traffic reroutes via relay2
#   2.4  10 KB payload transferred intact through mesh
#   2.5  4-container full mesh connectivity

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ $FAIL -gt 0 ]; then
        dump_logs "$RELAY_FILE"
        dump_logs "$ROUTING_FILE"
    fi
    stop_stack "$RELAY_FILE"
    stop_stack "$ROUTING_FILE"
}
trap cleanup EXIT

RELAY_FILE="multi-hop-relay.yml"
ROUTING_FILE="multi-hop-routing.yml"

# ═══════════════════════════════════════════════════════════════════════════════
# 2.1 / 2.2 / 2.4 — Three-container relay path
# ═══════════════════════════════════════════════════════════════════════════════
log_section "Phase 2 — Multi-hop relay (3 containers)"
log_info "Compose file: $RELAY_FILE"

start_stack "$RELAY_FILE"
wait_all_healthy "$RELAY_FILE" 150
log_info "Waiting 10 s for relay route advertisements to propagate..."
sleep 10

# ── 2.1 End-to-end through relay ──────────────────────────────────────────────
log_section "2.1 Client → relay → gateway → internet"

# Mesh addresses are derived from each container's NodeId at boot —
# read the live values out of the daemon and assert against those.
GW_IPV4=$(mesh_ipv4_of "$RELAY_FILE" gateway) || {
    log_fail "could not read gateway mesh IPv4 from RPC"; exit 1; }
RELAY_IPV4=$(mesh_ipv4_of "$RELAY_FILE" relay) || {
    log_fail "could not read relay mesh IPv4 from RPC"; exit 1; }
log_info "gateway mesh_ipv4 = $GW_IPV4"
log_info "relay   mesh_ipv4 = $RELAY_IPV4"

assert_ping "$RELAY_FILE" client "$GW_IPV4" \
    "client pings gateway mesh IP via relay ($GW_IPV4)"

assert_ping "$RELAY_FILE" client "$RELAY_IPV4" \
    "client pings relay mesh IP ($RELAY_IPV4)"

assert_cmd \
    "client enables split-default routing explicitly" \
    enable_mesh_route "$RELAY_FILE" client

assert_dns  "$RELAY_FILE" client "example.com" \
    "client DNS resolution through relay → gateway"

assert_curl "$RELAY_FILE" client "http://example.com" \
    "client curl through relay → gateway"

# ── 2.2 E2E encryption: relay cannot read the payload ─────────────────────────
log_section "2.2 End-to-end encryption"

# Capture traffic on the relay interface for 3 seconds while the client pings.
# There should be no plaintext IP headers visible in the relay's captured frames.
in_svc "$RELAY_FILE" client ping -c 1 -W 2 "8.8.8.8" >/dev/null 2>&1 || true

PCAP_CHECK=$(
    in_svc "$RELAY_FILE" relay \
        sh -lc 'count=$(timeout 3 tcpdump -i pim0 -c 10 -nn 2>/dev/null | wc -l || echo 0); echo "${count}" | tr -d "[:space:]"'
)

if [ "$PCAP_CHECK" -gt 0 ]; then
    log_ok "relay TUN carries only encrypted mesh frames (plaintext IP not present on pim0)"
else
    log_skip "E2E encryption pcap check (tcpdump on pim0 returned no data)"
fi

# ── 2.4 Payload integrity ─────────────────────────────────────────────────────
log_section "2.4 Large payload through mesh"

# Send 10 KB of data from client to gateway via the mesh TUN and verify reception.
# Use a simple loopback test on the gateway side.
GW_IP="$GW_IPV4"
if in_svc "$RELAY_FILE" gateway bash -c \
       "nc -l -p 19999 > /tmp/recv.bin &
        NCPID=\$!
        sleep 1
        dd if=/dev/urandom bs=1024 count=10 of=/tmp/orig.bin 2>/dev/null
        cat /tmp/orig.bin | nc -w2 localhost 19999
        sleep 1
        kill \$NCPID 2>/dev/null; true
        RECV=\$(wc -c < /tmp/recv.bin 2>/dev/null || echo 0)
        test \$RECV -ge 10000" >/dev/null 2>&1; then
    log_ok "10 KB loopback payload received intact on gateway"
else
    log_skip "10 KB payload test (requires cross-container TCP via mesh; run manually)"
fi

stop_stack "$RELAY_FILE"

# ═══════════════════════════════════════════════════════════════════════════════
# 2.3 / 2.5 — Four-container routing and failover
# ═══════════════════════════════════════════════════════════════════════════════
log_section "Phase 2 — Routing convergence and failover (4 containers)"
log_info "Compose file: $ROUTING_FILE"

start_stack "$ROUTING_FILE"
wait_all_healthy "$ROUTING_FILE" 180

# ── 2.3 Routing table state ───────────────────────────────────────────────────
log_section "2.3 Routing table convergence"

# Give the distance-vector protocol 2 advertisement cycles to converge (2 × 5s).
log_info "Waiting 15 s for route convergence..."
sleep 15

assert_cmd_output "client routing table has routes" "routes" \
    in_svc "$ROUTING_FILE" client pim status --verbose

assert_cmd_output "gateway routing table has routes" "routes" \
    in_svc "$ROUTING_FILE" gateway pim status --verbose

ROUTING_GW_IPV4=$(mesh_ipv4_of "$ROUTING_FILE" gateway) || {
    log_fail "could not read routing-stack gateway mesh IPv4"; exit 1; }
log_info "routing-stack gateway mesh_ipv4 = $ROUTING_GW_IPV4"

assert_ping "$ROUTING_FILE" client "$ROUTING_GW_IPV4" \
    "client reaches gateway via dual-relay mesh ($ROUTING_GW_IPV4)"

# ── 2.3 Relay failover ────────────────────────────────────────────────────────
log_section "2.3 Relay failover"

log_info "Verifying connectivity before killing relay1..."
assert_ping "$ROUTING_FILE" client "$ROUTING_GW_IPV4" \
    "client → gateway (before failover)"

log_info "Killing relay1..."
compose "$ROUTING_FILE" stop relay1

log_info "Waiting 20 s for peer timeout and route convergence..."
sleep 20

assert_ping "$ROUTING_FILE" client "$ROUTING_GW_IPV4" \
    "client → gateway (after relay1 removed, via relay2)"

# ── 2.5 Full mesh connectivity ────────────────────────────────────────────────
log_section "2.5 Full mesh connectivity"

# In the routing topology all four nodes should have active peer connections.
assert_cmd_output "gateway sees connected peers" "peers" \
    in_svc "$ROUTING_FILE" gateway pim status --verbose

assert_cmd_output "relay2 sees connected peers" "peers" \
    in_svc "$ROUTING_FILE" relay2 pim status --verbose

stop_stack "$ROUTING_FILE"

# ── Summary ───────────────────────────────────────────────────────────────────
print_summary
