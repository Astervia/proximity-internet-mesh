#!/usr/bin/env bash
# test-route-cli.sh — Docker system test for split-default route management.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

COMPOSE_FILE="phase1-single-hop.yml"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ $FAIL -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "Route CLI Docker lane"
log_info "Compose file: $COMPOSE_FILE"

start_stack "$COMPOSE_FILE"
wait_all_healthy "$COMPOSE_FILE" 120

log_section "Initial state"
assert_cmd_output \
    "client route status starts disabled" \
    "pim routes: disabled" \
    in_svc "$COMPOSE_FILE" client pim route status

log_section "Enable split-default routes"
assert_cmd \
    "client enables split-default routes" \
    in_svc "$COMPOSE_FILE" client pim route on

assert_cmd_output \
    "client route status reports enabled" \
    "pim routes: enabled via 10.77.0.1 dev pim0" \
    in_svc "$COMPOSE_FILE" client pim route status

assert_cmd_output \
    "lower-half split default installed" \
    "0.0.0.0/1 via 10.77.0.1 dev pim0" \
    in_svc "$COMPOSE_FILE" client ip route show

assert_cmd_output \
    "upper-half split default installed" \
    "128.0.0.0/1 via 10.77.0.1 dev pim0" \
    in_svc "$COMPOSE_FILE" client ip route show

log_section "Disable split-default routes"
assert_cmd \
    "client disables split-default routes" \
    in_svc "$COMPOSE_FILE" client pim route off

assert_cmd_output \
    "client route status reports disabled again" \
    "pim routes: disabled" \
    in_svc "$COMPOSE_FILE" client pim route status

assert_cmd \
    "lower-half split default removed" \
    in_svc "$COMPOSE_FILE" client bash -lc \
    "! ip route show | grep -q '0.0.0.0/1 via 10.77.0.1 dev pim0'"

assert_cmd \
    "upper-half split default removed" \
    in_svc "$COMPOSE_FILE" client bash -lc \
    "! ip route show | grep -q '128.0.0.0/1 via 10.77.0.1 dev pim0'"

print_summary
