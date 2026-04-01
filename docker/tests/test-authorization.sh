#!/usr/bin/env bash
# test-authorization.sh — authorization and keyed-discovery tests

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

ALLOW_ALL_FILE="auth-allow-all.yml"
ALLOW_LIST_FILE="auth-allow-list.yml"
TOFU_FILE="auth-tofu.yml"
DISCOVERY_KEY_FILE="auth-discovery-key.yml"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ $FAIL -gt 0 ]; then
        dump_logs "$ALLOW_ALL_FILE"
        dump_logs "$ALLOW_LIST_FILE"
        dump_logs "$TOFU_FILE"
        dump_logs "$DISCOVERY_KEY_FILE"
    fi
    stop_stack "$ALLOW_ALL_FILE"
    stop_stack "$ALLOW_LIST_FILE"
    stop_stack "$TOFU_FILE"
    stop_stack "$DISCOVERY_KEY_FILE"
}
trap cleanup EXIT

log_section "Authorization — allow_all"
start_stack "$ALLOW_ALL_FILE"
wait_all_healthy "$ALLOW_ALL_FILE" 120
wait_for_peers "$ALLOW_ALL_FILE" client 1 40
assert_peer_count "$ALLOW_ALL_FILE" client 1 \
    "allow_all client has one peer"
assert_ping "$ALLOW_ALL_FILE" client "10.77.0.1" \
    "allow_all client reaches gateway mesh IP"
stop_stack "$ALLOW_ALL_FILE"

log_section "Authorization — allow_list"
start_stack "$ALLOW_LIST_FILE"
wait_all_healthy "$ALLOW_LIST_FILE" 120
wait_for_peers "$ALLOW_LIST_FILE" client 1 50
sleep 8
assert_peer_count "$ALLOW_LIST_FILE" client 1 \
    "allow_list client formed an authorized session"
assert_peer_count "$ALLOW_LIST_FILE" intruder 0 \
    "allow_list intruder has no peers"
assert_logs_contain "$ALLOW_LIST_FILE" gateway "rejected by authorization policy" \
    "allow_list gateway rejects the unauthorized peer"
stop_stack "$ALLOW_LIST_FILE"

log_section "Authorization — trust_on_first_use"
start_stack "$TOFU_FILE"
wait_all_healthy "$TOFU_FILE" 120
wait_for_peers "$TOFU_FILE" client 1 50
sleep 6
assert_cmd_output "TOFU gateway trust store records the client" "6a3803d5f059902a1c6dafbc9ba47292" \
    in_svc "$TOFU_FILE" gateway cat /var/lib/pim/trusted-peers.toml
assert_cmd_output "TOFU client trust store records the gateway" "34750f98bd59fcfc946da45aaabe933b" \
    in_svc "$TOFU_FILE" client cat /var/lib/pim/trusted-peers.toml

log_info "Restarting TOFU client to verify persisted trust..."
compose "$TOFU_FILE" restart client >/dev/null
wait_healthy "$TOFU_FILE" client 90
wait_for_peers "$TOFU_FILE" client 1 50
TOFU_COUNT=$(compose "$TOFU_FILE" logs --no-color gateway client 2>/dev/null | grep -c "peer trusted on first use" || true)
if [ "${TOFU_COUNT:-0}" -eq 2 ]; then
    log_ok "TOFU trust persisted across restart"
else
    log_fail "TOFU trust persisted across restart (expected 2 trust-on-first-use log lines, got ${TOFU_COUNT:-0})"
fi
stop_stack "$TOFU_FILE"

log_section "Discovery — shared key"
start_stack "$DISCOVERY_KEY_FILE"
wait_all_healthy "$DISCOVERY_KEY_FILE" 120
wait_for_peers "$DISCOVERY_KEY_FILE" client 1 50
sleep 8
assert_cmd_output "keyed discovery client sees the gateway" "discovered peers: 1" \
    in_svc "$DISCOVERY_KEY_FILE" client pim debug discovery
assert_cmd_output "node without the discovery key sees no peers" "discovered peers: 0" \
    in_svc "$DISCOVERY_KEY_FILE" outsider pim debug discovery
assert_peer_count "$DISCOVERY_KEY_FILE" outsider 0 \
    "node without the discovery key has no sessions"
stop_stack "$DISCOVERY_KEY_FILE"

print_summary
