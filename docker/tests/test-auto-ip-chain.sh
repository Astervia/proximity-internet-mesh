#!/usr/bin/env bash
# test-auto-ip-chain.sh — Routed auto-IP chain with late gateway join
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

COMPOSE_FILE="auto-ip-chain.yml"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ "$FAIL" -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "Phase 8 — Late gateway join over a relay chain"
log_info "Compose file: $COMPOSE_FILE"
log_info "Topology: gateway <-> relay1 <-> relay2 <-> client"
log_info "Mesh IPs are derived from each node's NodeId — no allocation handshake."

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

log_section "8C All chain nodes have derived mesh IPs"

# Mesh IPs are derived from each NodeId at boot — no allocation
# handshake — so each pim0 carries `derive_mesh_ipv4(self_id)`.
GW_IPV4=$(mesh_ipv4_of "$COMPOSE_FILE" gateway) || {
    log_fail "could not read gateway mesh IPv4"; exit 1; }
RELAY1_IPV4=$(mesh_ipv4_of "$COMPOSE_FILE" relay1) || {
    log_fail "could not read relay1 mesh IPv4"; exit 1; }
RELAY2_IPV4=$(mesh_ipv4_of "$COMPOSE_FILE" relay2) || {
    log_fail "could not read relay2 mesh IPv4"; exit 1; }
CLIENT_IPV4=$(mesh_ipv4_of "$COMPOSE_FILE" client) || {
    log_fail "could not read client mesh IPv4"; exit 1; }
log_info "derived: gateway=$GW_IPV4 relay1=$RELAY1_IPV4 relay2=$RELAY2_IPV4 client=$CLIENT_IPV4"

assert_iface_up "$COMPOSE_FILE" relay1 "pim0" \
    "relay1 pim0 interface is UP after late gateway join"
assert_iface_addr "$COMPOSE_FILE" relay1 "$RELAY1_IPV4" \
    "relay1 has derived mesh IP $RELAY1_IPV4"

assert_iface_up "$COMPOSE_FILE" relay2 "pim0" \
    "relay2 pim0 interface is UP after late gateway join"
assert_iface_addr "$COMPOSE_FILE" relay2 "$RELAY2_IPV4" \
    "relay2 has derived mesh IP $RELAY2_IPV4"

assert_iface_up "$COMPOSE_FILE" client "pim0" \
    "client pim0 interface is UP after late gateway join"
assert_iface_addr "$COMPOSE_FILE" client "$CLIENT_IPV4" \
    "client has derived mesh IP $CLIENT_IPV4"

log_section "8D End-to-end connectivity through the chain"

assert_ping "$COMPOSE_FILE" client "$GW_IPV4" \
    "client can ping gateway mesh IP ($GW_IPV4) through relay1/relay2"

assert_cmd \
    "client enables split-default routing explicitly after routed auto-IP" \
    enable_mesh_route "$COMPOSE_FILE" client

assert_curl "$COMPOSE_FILE" client "http://example.com" \
    "client can curl http://example.com through late-joined gateway"

print_summary
