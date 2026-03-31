#!/usr/bin/env bash
# test-bluetooth.sh — Docker seam test for Bluetooth PAN automatic peer discovery.

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

log_info "Waiting for fake bluetoothctl scan, bt-network PAN setup, and neighbor discovery..."
sleep 5

assert_iface_up "$COMPOSE_FILE" node pim0 "daemon started and pim0 is UP"
assert_cmd_output \
    "fake sysfs operstate reached up" \
    "up" \
    in_svc "$COMPOSE_FILE" node cat /tmp/fake-sysfs/bnep0/operstate

assert_logs_contain \
    "$COMPOSE_FILE" node "Bluetooth radio-discovered peer prepared" \
    "daemon discovered and prepared a new peer over the fake Bluetooth radio layer"

assert_logs_contain \
    "$COMPOSE_FILE" node "Bluetooth PAN discovered peer addr" \
    "daemon auto-discovered a peer address from the fake PAN neighbor table"

assert_logs_contain \
    "$COMPOSE_FILE" node "Bluetooth PAN: peer addr ready — initiating connection" \
    "daemon handed Bluetooth-discovered addr into the normal connection path"

assert_cmd_output \
    "daemon status remains running under fake Bluetooth seam" \
    "running" \
    in_svc "$COMPOSE_FILE" node pim status

print_summary
