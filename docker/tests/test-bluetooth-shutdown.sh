#!/usr/bin/env bash
# test-bluetooth-shutdown.sh — verify pim-daemon cleanly tears down its
# Bluetooth gateway resources on SIGTERM.
#
# The node container runs `bluetooth-shutdown-probe.sh`, which launches
# pim-daemon, waits for the bridge and MASQUERADE rule to appear, sends
# SIGTERM, and asserts that everything is cleaned up. The probe's exit
# code propagates through docker compose, and this driver reports it.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

COMPOSE_FILE="bluetooth-shutdown.yml"
SERVICE="node"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ $FAIL -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "Bluetooth gateway shutdown cleanup"
log_info "Compose file: $COMPOSE_FILE"

start_stack "$COMPOSE_FILE"

log_info "Waiting for probe container to finish (timeout 90s)..."
container_id="$(compose "$COMPOSE_FILE" ps -q "$SERVICE")"
if [ -z "$container_id" ]; then
    log_fail "service $SERVICE was not created"
    print_summary
    exit 1
fi

# Block until the probe container exits, up to 90 seconds.
if ! timeout 90 docker wait "$container_id" >/tmp/pim-bt-shutdown-exit 2>/dev/null; then
    log_fail "probe container did not exit within 90s"
    compose "$COMPOSE_FILE" logs --no-color "$SERVICE" | tail -80 || true
    print_summary
    exit 1
fi

exit_code="$(cat /tmp/pim-bt-shutdown-exit 2>/dev/null || echo "?")"
log_info "probe container exited with code $exit_code"

# Stream the probe's stdout/stderr into our output so PASS/FAIL lines are visible.
compose "$COMPOSE_FILE" logs --no-color "$SERVICE" 2>&1 | sed 's/^/  [node] /'

if [ "$exit_code" = "0" ]; then
    log_ok "pim-daemon released bridge, iptables rules, and tcp/9100 on SIGTERM"
else
    log_fail "probe reported cleanup failure (exit $exit_code)"
fi

print_summary
