#!/usr/bin/env bash
# test-coc-discovery.sh — L2CAP CoC seam test (Docker-realistic scope).
#
# AF_BLUETOOTH + BTPROTO_L2CAP is per-network-namespace on Linux: a
# stock Docker bridge container does NOT inherit the host's bluetooth
# stack, so binding an L2CAP CoC listener returns EAFNOSUPPORT
# (os error 97). End-to-end CoC peering needs real hardware on both
# ends (the two-machine bench in `docs/research/` covers that) —
# this lab only validates that `pim-daemon` survives an unavailable
# Bluetooth stack cleanly:
#
#   1. The bluetooth_coc.start call fails with a recognisable WARN
#      ("coc service failed to start: bind L2CAP CoC psm … Address
#      family not supported by protocol").
#   2. pim-bluetooth (PAN/NAP) and bluetooth_rfcomm stay disabled per
#      config — no surprise controller setup or RFCOMM listener as a
#      side-effect of the CoC bind failure.
#   3. The daemon STILL boots — TUN up, transport listening, event
#      loop started, RPC listening — because CoC bind failure must
#      never block the rest of the daemon.
#   4. `pim status` keeps responding for the duration of the test.
#
# What we cannot test here: handshake, frame I/O, bridge stitching to
# loopback TCP, LE GAP advertising/scan. Add those once we have a
# hardware bench / privileged host-network lab.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

COMPOSE_FILE="coc-discovery.yml"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ "$FAIL" -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "L2CAP CoC seam — graceful degradation when AF_BLUETOOTH is unavailable"

start_stack "$COMPOSE_FILE"
wait_healthy "$COMPOSE_FILE" node 60

log_section "CoC bind reports the expected failure"

assert_logs_contain "$COMPOSE_FILE" node \
    "coc service failed to start" \
    "CoC service surfaces a startup-failure WARN"
assert_logs_contain "$COMPOSE_FILE" node \
    "Address family not supported by protocol" \
    "failure reason matches the AF_BLUETOOTH unavailability"

# Defensive: if a future change accidentally swallows the failure into
# a successful start, the next assertion catches it.
SUCCESS_LOG="$(compose "$COMPOSE_FILE" logs --no-color node 2>&1 | grep -c 'Bluetooth L2CAP CoC service started' || true)"
if [ "$SUCCESS_LOG" -eq 0 ]; then
    log_ok "no spurious 'CoC service started' line for a bind that failed"
else
    log_fail "logs claim CoC started but bind cannot have succeeded in this namespace"
fi

log_section "Sibling Bluetooth services stayed off per config"

PAN_LOG="$(compose "$COMPOSE_FILE" logs --no-color node 2>&1 | grep -c 'Bluetooth service starting' || true)"
if [ "$PAN_LOG" -eq 0 ]; then
    log_ok "PAN watcher stayed off (bluetooth.enabled=false respected)"
else
    log_fail "PAN watcher ran despite bluetooth.enabled=false"
fi

RFCOMM_START_LOG="$(compose "$COMPOSE_FILE" logs --no-color node 2>&1 | grep -c 'Bluetooth RFCOMM service started' || true)"
if [ "$RFCOMM_START_LOG" -eq 0 ]; then
    log_ok "RFCOMM service stayed off (bluetooth_rfcomm.enabled=false respected)"
else
    log_fail "RFCOMM service ran despite bluetooth_rfcomm.enabled=false"
fi

log_section "Daemon survived the CoC startup failure"

assert_logs_contain "$COMPOSE_FILE" node \
    "TUN up" \
    "TUN came up after CoC bind failure"
assert_logs_contain "$COMPOSE_FILE" node \
    "transport listening" \
    "TCP transport bound after CoC bind failure"
assert_logs_contain "$COMPOSE_FILE" node \
    "event loop started" \
    "main event loop reached after CoC bind failure"
assert_logs_contain "$COMPOSE_FILE" node \
    "rpc listening" \
    "JSON-RPC socket up after CoC bind failure"

log_section "RPC surface is live"

if in_svc "$COMPOSE_FILE" node pim status >/dev/null 2>&1; then
    log_ok "pim status returns successfully"
else
    log_fail "pim status RPC failed — the daemon is not actually serving"
fi

# Sleep a few seconds and confirm the daemon hasn't subsequently crashed
# after the initial boot (e.g. CoC background task panicking after
# scheduling).
sleep 4
if in_svc "$COMPOSE_FILE" node pim status >/dev/null 2>&1; then
    log_ok "pim status still responsive 4 s later — daemon stable"
else
    log_fail "pim status went unresponsive after the boot window"
fi

print_summary
