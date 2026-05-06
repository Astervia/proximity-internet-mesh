#!/usr/bin/env bash
# test-rfcomm-cleanup.sh — Phase 2 RFCOMM peer cleanup lab.
#
# Pre-seeds peers.db with one stale `rfcomm_peer_lifecycle` row
# (`first_paired_at_s` = now - 2 days), starts the daemon with
# `peer_cleanup_enabled = true`, and asserts the cleanup task:
#
#   1. Started at boot ("rfcomm peer cleanup task started").
#   2. Issued `bluetoothctl remove AA:BB:CC:DD:EE:FF` against the fake
#      shim within ~75 s (the cleanup interval is clamped to 60 s).
#   3. Logged the unpair at INFO with the stale peer's bd_addr.
#   4. Dropped the lifecycle row from peers.db.
#
# Like rfcomm-discovery, this lab does NOT exercise the real
# Bluetooth stack — `AF_BLUETOOTH` is per-netns. Phase 2 cleanup is
# independent of the RFCOMM service, so the daemon's RFCOMM bind
# failure here is tolerated, not the focus.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

COMPOSE_FILE="rfcomm-cleanup.yml"
SVC=node

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ "$FAIL" -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "RFCOMM cleanup — opt-in stale-peer unpair"

start_stack "$COMPOSE_FILE"
wait_healthy "$COMPOSE_FILE" "$SVC" 60

log_section "Cleanup task spawned"
assert_logs_contain "$COMPOSE_FILE" "$SVC" \
    "rfcomm peer cleanup task started" \
    "cleanup task started log line emitted"

log_section "Cleanup runs and removes the stale peer"

# The cleanup interval is clamped to 60 s. Wait up to ~80 s for the
# first tick + remove to land in the fake shim's log.
DEADLINE=$(( $(date +%s) + 80 ))
SAW_REMOVE=0
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
    if compose "$COMPOSE_FILE" exec -T "$SVC" cat /tmp/fake-bin/remove.log 2>/dev/null \
        | grep -q "remove AA:BB:CC:DD:EE:FF"; then
        SAW_REMOVE=1
        break
    fi
    sleep 2
done

if [ "$SAW_REMOVE" -eq 1 ]; then
    log_ok "fake bluetoothctl saw remove AA:BB:CC:DD:EE:FF"
else
    log_fail "cleanup never invoked bluetoothctl remove (waited 80 s)"
fi

log_section "Cleanup logged the unpair at INFO"
assert_logs_contain "$COMPOSE_FILE" "$SVC" \
    "rfcomm cleanup: unpaired unreachable peer" \
    "INFO log line emitted with structured cleanup reason"
assert_logs_contain "$COMPOSE_FILE" "$SVC" \
    "AA:BB:CC:DD:EE:FF" \
    "log carries the stale peer bd_addr"

log_section "Lifecycle row dropped from SQLite"
ROW_COUNT=$(compose "$COMPOSE_FILE" exec -T "$SVC" \
    sqlite3 /var/lib/pim/peers.db \
    "SELECT COUNT(*) FROM rfcomm_peer_lifecycle WHERE bd_addr='AA:BB:CC:DD:EE:FF';" 2>/dev/null \
    | tr -d '\r' || echo "")
if [ "$ROW_COUNT" = "0" ]; then
    log_ok "rfcomm_peer_lifecycle row was deleted after successful unpair"
else
    log_fail "lifecycle row still present (count=$ROW_COUNT) after cleanup"
fi

print_summary
