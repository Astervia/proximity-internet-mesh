#!/usr/bin/env bash
# test-phase8.sh — Routed auto-IP chain with late gateway join
#
# Topology:
#   gateway <-> relay1 <-> relay2 <-> client
#
# Goals:
#   8A relay1, relay2, and client all start with mesh_ip = "auto"
#   8B the gateway joins late and the chain converges
#   8C all auto peers receive a mesh IP after the gateway appears
#   8D client reaches gateway and internet through the chain

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

COMPOSE_FILE="phase8-auto-ip-chain.yml"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ "$FAIL" -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "Phase 8 — Routed auto-IP chain"
log_info "Compose file: $COMPOSE_FILE"
log_info "Topology: gateway <-> relay1 <-> relay2 <-> client"
log_info "relay1, relay2, and client all use mesh_ip = auto"

compose "$COMPOSE_FILE" down -v --remove-orphans >/dev/null 2>&1 || true

log_section "8A Start chain without gateway"
compose "$COMPOSE_FILE" up -d --build relay1 relay2 client
wait_healthy "$COMPOSE_FILE" relay1 90
wait_healthy "$COMPOSE_FILE" relay2 90
wait_healthy "$COMPOSE_FILE" client 90

log_info "Waiting 10 s for relay-only chain to stabilize before the gateway appears..."
sleep 10

assert_cmd_output "relay1 daemon is up before gateway join" "peers" \
    in_svc "$COMPOSE_FILE" relay1 pim status --verbose

assert_cmd_output "relay2 daemon is up before gateway join" "peers" \
    in_svc "$COMPOSE_FILE" relay2 pim status --verbose

assert_cmd_output "client daemon is up before gateway join" "peers" \
    in_svc "$COMPOSE_FILE" client pim status --verbose

log_section "8B Start gateway late"
compose "$COMPOSE_FILE" up -d gateway
wait_healthy "$COMPOSE_FILE" gateway 90

log_info "Waiting 45 s for reconnect, route convergence, and routed IP assignment..."
sleep 45

log_section "8C All auto peers receive mesh IPs"

assert_iface_up "$COMPOSE_FILE" relay1 "pim0" \
    "relay1 pim0 interface is UP after late gateway join"
assert_iface_addr "$COMPOSE_FILE" relay1 "10.77.0." \
    "relay1 received mesh IP after late gateway join"

assert_iface_up "$COMPOSE_FILE" relay2 "pim0" \
    "relay2 pim0 interface is UP after late gateway join"
assert_iface_addr "$COMPOSE_FILE" relay2 "10.77.0." \
    "relay2 received mesh IP through relay chain"

assert_iface_up "$COMPOSE_FILE" client "pim0" \
    "client pim0 interface is UP after late gateway join"
assert_iface_addr "$COMPOSE_FILE" client "10.77.0." \
    "client received mesh IP through two relays"

assert_logs_contain "$COMPOSE_FILE" relay1 "received IP assignment" \
    "relay1 logs show routed/late IP assignment" 45
assert_logs_contain "$COMPOSE_FILE" relay2 "received IP assignment" \
    "relay2 logs show routed IP assignment through relay1" 45
assert_logs_contain "$COMPOSE_FILE" client "received IP assignment" \
    "client logs show routed IP assignment through relay chain" 45

log_section "8D End-to-end connectivity through the chain"

assert_ping "$COMPOSE_FILE" client "10.77.0.1" \
    "client can ping gateway mesh IP through relay1/relay2"

assert_cmd \
    "client enables split-default routing explicitly after routed auto-IP" \
    enable_mesh_route "$COMPOSE_FILE" client

assert_curl "$COMPOSE_FILE" client "http://example.com" \
    "client can curl http://example.com through late-joined gateway"

print_summary
