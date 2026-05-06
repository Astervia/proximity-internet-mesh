#!/usr/bin/env bash
# test-messaging.sh — End-to-end user-to-user messaging through direct
# and routed paths.
#
# Topology (see docker/compose/mesh-broadcast.yml):
#
#   node-a ── node-b ── node-c ── node-d
#
# What this exercises:
#   1. messages.send to a direct neighbour (node-a → node-b).
#   2. messages.send across multi-hop routing (node-a → node-d, 3 hops).
#   3. ECIES round-trip — the recipient's `messages.history` returns the
#      decrypted plaintext we sent.
#   4. Delivery acks — sender's outbound message status transitions
#      `pending` → `sent` → `delivered` after the recipient processes
#      the `messaging.ack` PluginPayload and the routed reply lands.
#   5. messages.list_conversations attaches the peer-directory cached
#      `name` and `x25519_pubkey` to each row.
#   6. peers.forget triggers the messaging plugin's per-peer wipe via
#      `DaemonPlugin::on_peer_forgotten`.
#
# Note: messaging RPC is only compiled into the daemon when the
# `messaging` Cargo feature is enabled (default-on).

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

# Polls messages.history on `recv_svc` until a row matching
# `expected_body` shows up (or `max` seconds elapse). Returns the
# message id on stdout when found.
wait_for_received_message() {
    local recv_svc="$1" peer_id_hex="$2" expected_body="$3" max="${4:-30}"
    local elapsed=0
    while [ $elapsed -lt $max ]; do
        local resp
        resp=$(rpc_result "$COMPOSE_FILE" "$recv_svc" messages.history \
            "$(printf '{"peer_node_id":"%s","limit":50}' "$peer_id_hex")" \
            2>/dev/null || true)
        if [ -n "$resp" ]; then
            local id
            id=$(echo "$resp" | jq -er --arg body "$expected_body" \
                '.messages[] | select(.body == $body and .direction == "received") | .id' \
                2>/dev/null | head -n1)
            if [ -n "$id" ]; then
                echo "$id"
                return 0
            fi
        fi
        sleep 1
        elapsed=$((elapsed+1))
    done
    return 1
}

# Polls messages.history on `send_svc` until the message with `msg_id`
# transitions to `expected_status`.
wait_for_status() {
    local send_svc="$1" peer_id_hex="$2" msg_id="$3" expected_status="$4" max="${5:-15}"
    local elapsed=0
    while [ $elapsed -lt $max ]; do
        local resp
        resp=$(rpc_result "$COMPOSE_FILE" "$send_svc" messages.history \
            "$(printf '{"peer_node_id":"%s","limit":50}' "$peer_id_hex")" \
            2>/dev/null || true)
        if [ -n "$resp" ]; then
            local status
            status=$(echo "$resp" | jq -er --arg id "$msg_id" \
                '.messages[] | select(.id == $id) | .status' \
                2>/dev/null | head -n1)
            if [ "$status" = "$expected_status" ]; then
                return 0
            fi
        fi
        sleep 1
        elapsed=$((elapsed+1))
    done
    return 1
}

log_section "Messaging — direct + multi-hop (4 containers)"
log_info "Compose file: $COMPOSE_FILE"

start_stack "$COMPOSE_FILE"
wait_all_healthy "$COMPOSE_FILE" 180

# ── Setup: collect ids and wait for routing + broadcast ──────────────────────
log_section "0. Routing convergence + identity broadcast"

NODE_A_ID=$(rpc_node_id "$COMPOSE_FILE" node-a)
NODE_B_ID=$(rpc_node_id "$COMPOSE_FILE" node-b)
NODE_D_ID=$(rpc_node_id "$COMPOSE_FILE" node-d)
if [ -z "$NODE_A_ID" ] || [ -z "$NODE_B_ID" ] || [ -z "$NODE_D_ID" ]; then
    log_fail "couldn't read NodeIds via rpc.status"
    exit 1
fi
log_info "node-a id: ${NODE_A_ID:0:8}..."
log_info "node-b id: ${NODE_B_ID:0:8}..."
log_info "node-d id: ${NODE_D_ID:0:8}..."

wait_routes "$COMPOSE_FILE" node-a 3 90
wait_routes "$COMPOSE_FILE" node-d 3 90

# Force one round of identity broadcasts so all nodes have x25519 keys
# for everyone in the routing table — `messages.send` requires the
# recipient's key to be cached locally.
rpc_result "$COMPOSE_FILE" node-a peers.broadcast_identity_now >/dev/null
rpc_result "$COMPOSE_FILE" node-b peers.broadcast_identity_now >/dev/null
rpc_result "$COMPOSE_FILE" node-c peers.broadcast_identity_now >/dev/null
rpc_result "$COMPOSE_FILE" node-d peers.broadcast_identity_now >/dev/null
sleep 3

# ── 1. Direct-neighbour messaging (node-a → node-b) ──────────────────────────
log_section "1. messages.send to a direct neighbour"

DIRECT_BODY="hello from a, $(date +%s)"
SEND_PARAMS=$(printf '{"peer_node_id":"%s","body":%s}' "$NODE_B_ID" "$(printf '%s' "$DIRECT_BODY" | jq -Rs .)")
SEND_RESULT=$(rpc_result "$COMPOSE_FILE" node-a messages.send "$SEND_PARAMS" || true)
DIRECT_MSG_ID=$(echo "$SEND_RESULT" | jq -r '.id // empty')
if [ -n "$DIRECT_MSG_ID" ] && [ "${#DIRECT_MSG_ID}" = "32" ]; then
    log_ok "messages.send (a→b) returned id ${DIRECT_MSG_ID:0:8}..."
else
    log_fail "messages.send (a→b) returned no id (got: $SEND_RESULT)"
fi

# Recipient picks up the decrypted body.
RECV_ID=$(wait_for_received_message node-b "$NODE_A_ID" "$DIRECT_BODY" 15 || true)
if [ -n "$RECV_ID" ] && [ "$RECV_ID" = "$DIRECT_MSG_ID" ]; then
    log_ok "node-b received the message with matching id and decrypted body"
elif [ -n "$RECV_ID" ]; then
    log_fail "node-b received a message but id mismatched (sent ${DIRECT_MSG_ID:0:8}, got ${RECV_ID:0:8})"
else
    log_fail "node-b never received '$DIRECT_BODY' from node-a within 15s"
fi

# Sender sees the delivery ack land.
if wait_for_status node-a "$NODE_B_ID" "$DIRECT_MSG_ID" "delivered" 15; then
    log_ok "node-a's outbound row transitions to 'delivered' after node-b acks"
else
    log_fail "node-a never observed delivery ack for ${DIRECT_MSG_ID:0:8}..."
fi

# ── 2. Multi-hop messaging (node-a → node-d, 3 hops) ─────────────────────────
log_section "2. messages.send across 3 hops"

ROUTED_BODY="multi-hop hi, $(date +%s)"
SEND_PARAMS_AD=$(printf '{"peer_node_id":"%s","body":%s}' "$NODE_D_ID" "$(printf '%s' "$ROUTED_BODY" | jq -Rs .)")
SEND_RESULT_AD=$(rpc_result "$COMPOSE_FILE" node-a messages.send "$SEND_PARAMS_AD" || true)
ROUTED_MSG_ID=$(echo "$SEND_RESULT_AD" | jq -r '.id // empty')
if [ -n "$ROUTED_MSG_ID" ] && [ "${#ROUTED_MSG_ID}" = "32" ]; then
    log_ok "messages.send (a→d) returned id ${ROUTED_MSG_ID:0:8}..."
else
    log_fail "messages.send (a→d) returned no id (got: $SEND_RESULT_AD)"
fi

RECV_ID_D=$(wait_for_received_message node-d "$NODE_A_ID" "$ROUTED_BODY" 30 || true)
if [ -n "$RECV_ID_D" ] && [ "$RECV_ID_D" = "$ROUTED_MSG_ID" ]; then
    log_ok "node-d received the routed message with matching id"
else
    log_fail "node-d never received '$ROUTED_BODY' from node-a within 30s"
fi

if wait_for_status node-a "$NODE_D_ID" "$ROUTED_MSG_ID" "delivered" 20; then
    log_ok "node-a sees the multi-hop ack land (status=delivered)"
else
    log_fail "node-a never saw delivery ack for the routed message"
fi

# ── 3. messages.list_conversations attaches the peer-directory data ──────────
log_section "3. list_conversations enriches rows from the peer directory"

CONV_RESP=$(rpc_result "$COMPOSE_FILE" node-a messages.list_conversations || true)
CONV_FOR_D=$(echo "$CONV_RESP" | jq -er --arg id "$NODE_D_ID" \
    '.conversations[] | select(.peer_node_id == $id)' 2>/dev/null || true)
if [ -n "$CONV_FOR_D" ]; then
    PUBKEY=$(echo "$CONV_FOR_D" | jq -r '.x25519_pubkey // empty')
    NAME=$(echo "$CONV_FOR_D" | jq -r '.name // empty')
    if [ -n "$PUBKEY" ] && [ "$PUBKEY" != "null" ] && [ "${#PUBKEY}" = "64" ]; then
        log_ok "node-a's conversation with node-d carries a 64-char x25519_pubkey from the keystore"
    else
        log_fail "x25519_pubkey missing/invalid in conversation row (got: ${PUBKEY:-<empty>})"
    fi
    if [ "$NAME" = "node-d" ]; then
        log_ok "conversation row uses peer name 'node-d' from the directory"
    else
        log_warn "conversation .name is '$NAME' (expected 'node-d' from PeerInfo)"
    fi
else
    log_fail "node-a's conversations list missing entry for ${NODE_D_ID:0:8}"
fi

# ── 4. peers.forget propagates to the messaging plugin ───────────────────────
log_section "4. peers.forget wipes the messaging plugin's per-peer state"

FORGET_PARAMS=$(printf '{"node_id":"%s"}' "$NODE_B_ID")
FORGET_RESULT=$(rpc_result "$COMPOSE_FILE" node-a peers.forget "$FORGET_PARAMS" || true)
FORGOT=$(echo "$FORGET_RESULT" | jq -r '.forgot_identity // false')
if [ "$FORGOT" = "true" ]; then
    log_ok "peers.forget(node-b) acked forgot_identity=true"
else
    log_fail "peers.forget(node-b) did not report forgot_identity=true (got: $FORGET_RESULT)"
fi

# Give the daemon a moment to fan the on_peer_forgotten callback
# through to the messaging plugin (it wipes the conversation in a
# spawn_blocking).
sleep 1

# After forget, node-a's conversations list should no longer contain
# node-b (the messaging plugin's storage row got dropped).
CONV_AFTER=$(rpc_result "$COMPOSE_FILE" node-a messages.list_conversations || true)
STILL_THERE=$(echo "$CONV_AFTER" | jq -er --arg id "$NODE_B_ID" \
    '.conversations[] | select(.peer_node_id == $id) | .peer_node_id' 2>/dev/null || true)
if [ -z "$STILL_THERE" ]; then
    log_ok "node-a's conversation with node-b was wiped via on_peer_forgotten"
else
    log_fail "node-a still has a conversation with node-b after peers.forget"
fi

# ── 5. Reject empty body / unknown peer ──────────────────────────────────────
log_section "5. messages.send guard rails"

EMPTY_PARAMS=$(printf '{"peer_node_id":"%s","body":""}' "$NODE_D_ID")
EMPTY_ERR=$(rpc_error "$COMPOSE_FILE" node-a messages.send "$EMPTY_PARAMS" || true)
if echo "$EMPTY_ERR" | grep -qi "non-empty"; then
    log_ok "empty body is rejected with INVALID_PARAMS"
else
    log_fail "empty body did not produce expected error (got: ${EMPTY_ERR:-<empty>})"
fi

UNKNOWN_PEER="ffffffffffffffffffffffffffffffff"
UNKNOWN_PARAMS=$(printf '{"peer_node_id":"%s","body":"hi"}' "$UNKNOWN_PEER")
UNKNOWN_ERR=$(rpc_error "$COMPOSE_FILE" node-a messages.send "$UNKNOWN_PARAMS" || true)
if echo "$UNKNOWN_ERR" | grep -qi "no x25519"; then
    log_ok "unknown peer is rejected with MESSAGE_PEER_UNKNOWN"
else
    log_fail "unknown peer did not produce expected error (got: ${UNKNOWN_ERR:-<empty>})"
fi

# ── Summary ──────────────────────────────────────────────────────────────────
print_summary
