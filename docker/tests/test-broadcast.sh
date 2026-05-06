#!/usr/bin/env bash
# test-broadcast.sh — Daemon identity-broadcast across a multi-hop mesh.
#
# Topology (see docker/compose/mesh-broadcast.yml):
#
#   node-a ── node-b ── node-c ── node-d
#
# What this exercises:
#   1. Routing convergence: every node ends up with N-1 destinations in
#      its routing table (here N = 4).
#   2. Direct PeerInfo (over the Noise session): each node's directly
#      connected neighbour shows up in `peers.list` with an X25519 key.
#   3. Routed PeerInfo (the broadcast cycle): non-adjacent peers learn
#      each other's identities through `peers.broadcast_identity_now`,
#      not through direct sessions. Verified by sending a `messages.send`
#      to a 3-hop peer — it would fail with `MESSAGE_PEER_UNKNOWN` if the
#      X25519 key never reached the keystore.
#   4. `peers.get_broadcast_state` reflects the most recent cycle:
#      `last_recipient_count` matches the number of routed destinations.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

COMPOSE_FILE="mesh-broadcast.yml"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ "$FAIL" -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "Broadcast — multi-hop chain (4 containers)"
log_info "Compose file: $COMPOSE_FILE"

start_stack "$COMPOSE_FILE"
wait_all_healthy "$COMPOSE_FILE" 180

# ── 0. Collect node ids ──────────────────────────────────────────────────────
log_section "0. Collect each node's NodeId"

NODE_A_ID=$(rpc_node_id "$COMPOSE_FILE" node-a)
NODE_B_ID=$(rpc_node_id "$COMPOSE_FILE" node-b)
NODE_C_ID=$(rpc_node_id "$COMPOSE_FILE" node-c)
NODE_D_ID=$(rpc_node_id "$COMPOSE_FILE" node-d)

if [ -z "$NODE_A_ID" ] || [ -z "$NODE_B_ID" ] || [ -z "$NODE_C_ID" ] || [ -z "$NODE_D_ID" ]; then
    log_fail "couldn't read all four NodeIds via rpc.status"
    exit 1
fi
log_info "node-a id: ${NODE_A_ID:0:8}..."
log_info "node-b id: ${NODE_B_ID:0:8}..."
log_info "node-c id: ${NODE_C_ID:0:8}..."
log_info "node-d id: ${NODE_D_ID:0:8}..."

# ── 1. Routing convergence ───────────────────────────────────────────────────
log_section "1. Routing convergence"

# 4-node chain → each node should reach 3 others (RouteUpdate cadence is
# 5 s; allow up to 3 cycles + headroom).
wait_routes "$COMPOSE_FILE" node-a 3 90
wait_routes "$COMPOSE_FILE" node-b 3 90
wait_routes "$COMPOSE_FILE" node-c 3 90
wait_routes "$COMPOSE_FILE" node-d 3 90

# ── 2. Direct PeerInfo (Noise-session-bound) ─────────────────────────────────
log_section "2. Direct PeerInfo seeds the keystore for adjacent peers"

# After the Noise handshake, each side emits a PeerInfo to the other, so
# `peers.list` (which only enumerates *direct sessions*) carries the
# remote x25519_pubkey straight away.
DIRECT_KEY=$(rpc_result "$COMPOSE_FILE" node-a peers.list \
    | jq -er --arg id "$NODE_B_ID" '.[] | select(.node_id == $id) | .x25519_pubkey // empty')
if [ -n "$DIRECT_KEY" ] && [ "$DIRECT_KEY" != "null" ]; then
    log_ok "node-a sees node-b's x25519 pubkey from the direct PeerInfo"
else
    log_fail "node-a missing node-b's x25519 pubkey from peers.list (got: ${DIRECT_KEY:-<empty>})"
fi

# Mirror check from node-d's side.
DIRECT_KEY_D=$(rpc_result "$COMPOSE_FILE" node-d peers.list \
    | jq -er --arg id "$NODE_C_ID" '.[] | select(.node_id == $id) | .x25519_pubkey // empty')
if [ -n "$DIRECT_KEY_D" ] && [ "$DIRECT_KEY_D" != "null" ]; then
    log_ok "node-d sees node-c's x25519 pubkey from the direct PeerInfo"
else
    log_fail "node-d missing node-c's x25519 pubkey from peers.list (got: ${DIRECT_KEY_D:-<empty>})"
fi

# ── 3. Routed PeerInfo (via the broadcast cycle) ─────────────────────────────
log_section "3. Routed broadcast → multi-hop peers learn each other"

# Trigger one explicit broadcast on each node so the test does not have
# to wait for the periodic 30 s tick.
log_info "triggering peers.broadcast_identity_now on every node"
rpc_result "$COMPOSE_FILE" node-a peers.broadcast_identity_now >/dev/null
rpc_result "$COMPOSE_FILE" node-b peers.broadcast_identity_now >/dev/null
rpc_result "$COMPOSE_FILE" node-c peers.broadcast_identity_now >/dev/null
rpc_result "$COMPOSE_FILE" node-d peers.broadcast_identity_now >/dev/null

# Allow 3 s for the routed PluginPayload-less PeerInfo frames to propagate
# through the chain (3 hops → ~3 RTT on a local Docker network).
sleep 3

# ── 3a. peers.get_broadcast_state reports the cycle outcome ──────────────────
BC_STATE_A=$(rpc_result "$COMPOSE_FILE" node-a peers.get_broadcast_state)
LAST_RECIPIENTS_A=$(echo "$BC_STATE_A" | jq -r '.last_recipient_count // 0')
if [ "${LAST_RECIPIENTS_A:-0}" = "3" ]; then
    log_ok "node-a's last broadcast reached 3 routed peers"
else
    log_fail "node-a last_recipient_count=${LAST_RECIPIENTS_A:-?} (expected 3)"
    echo "$BC_STATE_A" | jq . 2>/dev/null || echo "$BC_STATE_A"
fi

BC_STATE_D=$(rpc_result "$COMPOSE_FILE" node-d peers.get_broadcast_state)
LAST_RECIPIENTS_D=$(echo "$BC_STATE_D" | jq -r '.last_recipient_count // 0')
if [ "${LAST_RECIPIENTS_D:-0}" = "3" ]; then
    log_ok "node-d's last broadcast reached 3 routed peers"
else
    log_fail "node-d last_recipient_count=${LAST_RECIPIENTS_D:-?} (expected 3)"
fi

# ── 3b. Probe the keystore via messages.send to a 3-hop peer ─────────────────
#
# `peers.list` only enumerates direct sessions, so it cannot prove that
# node-a learned node-d's x25519 from a routed PeerInfo. Instead we use
# the messaging plugin: `messages.send` requires the recipient's X25519
# key to be cached locally and a route to exist. If the broadcast cycle
# never delivered node-d's PeerInfo to node-a, this fails with
# MESSAGE_PEER_UNKNOWN (-32060).
PROBE_PARAMS=$(printf '{"peer_node_id":"%s","body":"broadcast-probe"}' "$NODE_D_ID")
PROBE_ERR=$(rpc_error "$COMPOSE_FILE" node-a messages.send "$PROBE_PARAMS" || true)
if [ -z "$PROBE_ERR" ]; then
    log_ok "node-a → node-d (3 hops) messages.send succeeds — keystore was seeded by routed broadcast"
else
    log_fail "node-a → node-d messages.send failed: $PROBE_ERR"
fi

# Probe in the opposite direction too — broadcasts are symmetric.
PROBE_PARAMS_DA=$(printf '{"peer_node_id":"%s","body":"broadcast-probe"}' "$NODE_A_ID")
PROBE_ERR_DA=$(rpc_error "$COMPOSE_FILE" node-d messages.send "$PROBE_PARAMS_DA" || true)
if [ -z "$PROBE_ERR_DA" ]; then
    log_ok "node-d → node-a (3 hops) messages.send succeeds — keystore was seeded by routed broadcast"
else
    log_fail "node-d → node-a messages.send failed: $PROBE_ERR_DA"
fi

# ── 3c. Routed broadcast hits an interior 2-hop peer too ─────────────────────
PROBE_PARAMS_AC=$(printf '{"peer_node_id":"%s","body":"broadcast-probe-2hop"}' "$NODE_C_ID")
PROBE_ERR_AC=$(rpc_error "$COMPOSE_FILE" node-a messages.send "$PROBE_PARAMS_AC" || true)
if [ -z "$PROBE_ERR_AC" ]; then
    log_ok "node-a → node-c (2 hops) messages.send succeeds"
else
    log_fail "node-a → node-c messages.send failed: $PROBE_ERR_AC"
fi

# ── Summary ──────────────────────────────────────────────────────────────────
print_summary
