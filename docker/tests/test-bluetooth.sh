#!/usr/bin/env bash
# test-bluetooth.sh — Docker seam test for Bluetooth PAN readiness handling.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

COMPOSE_FILE="bluetooth-seam.yml"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ $FAIL -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "Bluetooth Docker seam"
log_info "Compose file: $COMPOSE_FILE"

start_stack "$COMPOSE_FILE"
wait_all_healthy "$COMPOSE_FILE" 120

log_info "Waiting for fake bnep0 operstate transition and watcher processing..."
sleep 5

assert_iface_up "$COMPOSE_FILE" node pim0 "daemon started and pim0 is UP"
assert_cmd_output \
    "fake sysfs operstate reached up" \
    "up" \
    in_svc "$COMPOSE_FILE" node cat /tmp/fake-sysfs/bnep0/operstate

assert_logs_contain \
    "$COMPOSE_FILE" node "Bluetooth PAN peer ready" \
    "daemon observed fake Bluetooth PAN interface readiness"

assert_logs_contain \
    "$COMPOSE_FILE" node "Bluetooth PAN: peer addr ready — initiating connection" \
    "daemon handed Bluetooth-discovered addr into the normal connection path"

assert_cmd_output \
    "daemon status remains running under fake Bluetooth seam" \
    "running" \
    in_svc "$COMPOSE_FILE" node pim status

print_summary
