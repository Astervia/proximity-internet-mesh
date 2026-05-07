#!/usr/bin/env bash
# test-private-mesh.sh — passphrase-based private mesh tests.
#
# Stack: gateway + client share `[mesh] mode = "private"` with the same
# passphrase + mesh_id. `intruder` uses the same mesh_id but a wrong
# passphrase. `outsider` runs the open mesh (no `[mesh]` section). All
# four sit on the same broadcast domain.
#
# Expected behaviour:
#   - gateway and client form a single session and can ping each other.
#   - intruder cannot decrypt advertisements, never adds a peer.
#   - outsider's plaintext advertisements are ignored by the private
#     pair, and outsider can't decrypt theirs — also no peers.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

PRIVATE_MESH_FILE="private-mesh.yml"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ $FAIL -gt 0 ]; then
        dump_logs "$PRIVATE_MESH_FILE"
    fi
    stop_stack "$PRIVATE_MESH_FILE"
}
trap cleanup EXIT

log_section "Private mesh — passphrase happy path"
start_stack "$PRIVATE_MESH_FILE"
wait_all_healthy "$PRIVATE_MESH_FILE" 150

# Give the discovery loop a few broadcast cycles + at least one
# Argon2id-derived handshake to settle.
wait_for_peers "$PRIVATE_MESH_FILE" client 1 60
sleep 6

assert_peer_count "$PRIVATE_MESH_FILE" client 1 \
    "client formed a session with the gateway"
assert_peer_count "$PRIVATE_MESH_FILE" gateway 1 \
    "gateway sees exactly the client (intruder + outsider rejected)"
assert_ping "$PRIVATE_MESH_FILE" client "10.77.0.1" \
    "client reaches the gateway over the private mesh"
assert_logs_contain "$PRIVATE_MESH_FILE" gateway "private mesh enabled" \
    "gateway logs the private-mesh fingerprint at startup"
assert_logs_contain "$PRIVATE_MESH_FILE" client "private mesh enabled" \
    "client logs the private-mesh fingerprint at startup"

log_section "Private mesh — wrong passphrase is invisible"
assert_peer_count "$PRIVATE_MESH_FILE" intruder 0 \
    "intruder with wrong passphrase has no peers"

log_section "Private mesh — open node cannot interop"
assert_peer_count "$PRIVATE_MESH_FILE" outsider 0 \
    "open-mesh outsider has no peers next to the private pair"

print_summary
