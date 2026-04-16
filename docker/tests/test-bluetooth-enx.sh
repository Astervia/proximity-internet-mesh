#!/usr/bin/env bash
# test-bluetooth-enx.sh — Docker seam test for dynamic Bluetooth PAN enx* fallback.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

COMPOSE_FILE="bluetooth-seam-enx.yml"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ $FAIL -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "Bluetooth Docker seam (dynamic enx)"
log_info "Compose file: $COMPOSE_FILE"

start_stack "$COMPOSE_FILE"
wait_all_healthy "$COMPOSE_FILE" 120

log_info "Waiting for fake bluetoothctl scan, bt-network PAN setup, dynamic enx interface resolution, and neighbor discovery..."
sleep 5

assert_iface_up "$COMPOSE_FILE" node pim0 "daemon started and pim0 is UP"
assert_cmd_output \
    "fake sysfs dynamic enx operstate reached up" \
    "up" \
    in_svc "$COMPOSE_FILE" node cat /tmp/fake-sysfs/enx6432a8144f4b/operstate

assert_logs_contain \
    "$COMPOSE_FILE" node "Bluetooth PAN interface selected" \
    "daemon selected a runtime PAN interface dynamically"

assert_logs_contain \
    "$COMPOSE_FILE" node "enx6432a8144f4b" \
    "daemon logs mention the live enx PAN interface instead of relying on the configured bnep hint"

assert_logs_contain \
    "$COMPOSE_FILE" node "dynamic-enx" \
    "daemon reported dynamic-enx as the interface selection source"

assert_logs_contain \
    "$COMPOSE_FILE" node "Bluetooth PAN discovered peer addr" \
    "daemon auto-discovered a peer address from the fake enx PAN neighbor table"

assert_logs_contain \
    "$COMPOSE_FILE" node "Bluetooth PAN: peer addr ready — initiating connection" \
    "daemon handed the enx-discovered addr into the normal connection path"

assert_cmd_output \
    "daemon status remains running under dynamic enx Bluetooth seam" \
    "running" \
    in_svc "$COMPOSE_FILE" node pim status

print_summary
