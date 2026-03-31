#!/usr/bin/env bash
# test-debug-cli.sh — Docker assertions for `pim debug ...` command output.
#
# Coverage:
#   - client-side debug output in a multi-gateway mesh
#   - route explanation output for internet and mesh destinations
#   - discovery output in a multi-gateway mesh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

PHASE5_FILE="phase5-multigateway.yml"
cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ $FAIL -gt 0 ]; then
        dump_logs "$PHASE5_FILE"
    fi
    stop_stack "$PHASE5_FILE"
}
trap cleanup EXIT

log_section "Debug CLI — Multi-gateway client view"
log_info "Starting stack without rebuild: $PHASE5_FILE"
compose "$PHASE5_FILE" up -d
wait_all_healthy "$PHASE5_FILE" 180
log_info "Waiting 15 s for route advertisements and debug snapshots..."
sleep 15

assert_cmd \
    "client writes structured debug snapshot" \
    in_svc "$PHASE5_FILE" client test -s /run/pim-debug.json

assert_cmd \
    "client debug peers shows relay and both gateways as direct TCP peers" \
    in_svc "$PHASE5_FILE" client bash -lc \
    'out="$(pim debug peers)" &&
     echo "$out" | grep -q "connected peers: 3" &&
     [ "$(echo "$out" | grep -c "direct=true")" -eq 3 ] &&
     [ "$(echo "$out" | grep -c "mechanism=tcp")" -eq 3 ]'

assert_cmd \
    "client debug routes lists relay and both gateways" \
    in_svc "$PHASE5_FILE" client bash -lc \
    'out="$(pim debug routes)" &&
     echo "$out" | grep -q "installed routes: 3" &&
     echo "$out" | grep -q "mesh_ip=10.77.0.10" &&
     echo "$out" | grep -q "mesh_ip=10.77.0.1" &&
     echo "$out" | grep -q "mesh_ip=10.77.0.2"'

assert_cmd \
    "client debug gateways lists two gateways and marks one selected" \
    in_svc "$PHASE5_FILE" client bash -lc \
    'out="$(pim debug gateways)" &&
     echo "$out" | grep -q "known gateways: 2" &&
     echo "$out" | grep -q "mesh_ip=10.77.0.1" &&
     echo "$out" | grep -q "mesh_ip=10.77.0.2" &&
     echo "$out" | grep -Eq "^\* "'

assert_cmd \
    "client debug route get internet explains the selected mesh egress" \
    in_svc "$PHASE5_FILE" client bash -lc \
    'out="$(pim debug route get internet)" &&
     echo "$out" | grep -q "^internet route:" &&
     echo "$out" | grep -q "gateway:" &&
     echo "$out" | grep -q "next_hop:" &&
     echo "$out" | grep -q "mechanism: tcp"'

assert_cmd \
    "client debug route get 10.77.0.2 explains a gateway destination route" \
    in_svc "$PHASE5_FILE" client bash -lc \
    'out="$(pim debug route get 10.77.0.2)" &&
     echo "$out" | grep -q "^route:" &&
     echo "$out" | grep -q "gateway:     true" &&
     echo "$out" | grep -q "mesh_ip:     10.77.0.2" &&
     echo "$out" | grep -q "mechanism:   tcp"'

assert_cmd \
    "client debug discovery shows both gateways plus the relay role mix" \
    in_svc "$PHASE5_FILE" client bash -lc \
    'out="$(pim debug discovery)" &&
     echo "$out" | grep -q "discovered peers: 3" &&
     [ "$(echo "$out" | grep -c "gateway=true")" -eq 2 ] &&
     echo "$out" | grep -q "relay=false"'

print_summary
