#!/usr/bin/env bash
# common.sh — shared helpers for PIM Docker integration test scripts.
# Source this file; do not execute directly.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m'

PASS=0
FAIL=0
SKIP=0

# ── Logging ───────────────────────────────────────────────────────────────────

log_info()  { echo -e "${BLUE}[INFO]${NC}  $*"; }
log_ok()    { echo -e "${GREEN}[PASS]${NC}  $*"; PASS=$((PASS+1)); }
log_fail()  { echo -e "${RED}[FAIL]${NC}  $*"; FAIL=$((FAIL+1)); }
log_warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
log_skip()  { echo -e "${YELLOW}[SKIP]${NC}  $*"; SKIP=$((SKIP+1)); }
log_section() { echo -e "\n${BOLD}── $* ──${NC}"; }

# ── Assertion helpers ─────────────────────────────────────────────────────────

# assert_cmd <description> <cmd...>
assert_cmd() {
    local desc="$1"; shift
    if "$@" >/dev/null 2>&1; then
        log_ok "$desc"
        return 0
    else
        log_fail "$desc"
        return 1
    fi
}

# assert_cmd_output <description> <expected_substring> <cmd...>
assert_cmd_output() {
    local desc="$1"
    local expected="$2"
    shift 2
    local output
    output=$("$@" 2>&1) || true
    if echo "$output" | grep -q "$expected"; then
        log_ok "$desc"
        return 0
    else
        log_fail "$desc (expected '$expected' in output, got: $output)"
        return 1
    fi
}

# assert_peer_count <file> <service> <expected> [desc]
assert_peer_count() {
    local file="$1" svc="$2" expected="$3"
    local desc="${4:-$svc has $expected peer(s)}"
    local count
    count=$(in_svc "$file" "$svc" pim status --verbose 2>/dev/null \
        | awk -F'[:=]' '/^[[:space:]]*peers[=:]/ {gsub(/[[:space:]]/, "", $2); print $2; exit}')
    count="${count:-0}"
    if [ "$count" = "$expected" ]; then
        log_ok "$desc"
    else
        log_fail "$desc (expected $expected, got ${count:-missing})"
    fi
}

# assert_logs_contain <file> <service> <expected_substring> [desc]
assert_logs_contain() {
    local file="$1" svc="$2" expected="$3"
    local desc="${4:-logs for $svc contain $expected}"
    local max="${5:-20}"
    local elapsed=0
    local output
    while [ $elapsed -lt $max ]; do
        output=$(docker compose -f "$COMPOSE_DIR/$file" logs --no-color "$svc" 2>&1) || true
        # Avoid piping a large string into `grep -q` — grep exits on first match,
        # triggers SIGPIPE on the left side, and `pipefail` surfaces that as a
        # failed pipeline even though the match was found.
        if [[ "$output" == *"$expected"* ]]; then
            log_ok "$desc"
            return 0
        fi
        sleep 1
        elapsed=$((elapsed+1))
    done
    log_fail "$desc (expected '$expected' in logs within ${max}s)"
    return 1
}

# ── Docker helpers ────────────────────────────────────────────────────────────

COMPOSE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../compose" && pwd)"

# compose <file> <args...>  — run docker compose with a named file
compose() {
    local file="$1"; shift
    docker compose -f "$COMPOSE_DIR/$file" "$@"
}

# in_svc <file> <service> <cmd...>  — exec into a running service
in_svc() {
    local file="$1"
    local svc="$2"
    shift 2
    docker compose -f "$COMPOSE_DIR/$file" exec -T "$svc" "$@"
}

# enable_mesh_route <file> <service> [config]
enable_mesh_route() {
    local file="$1" svc="$2" config="${3:-/etc/pim/pim.toml}"
    in_svc "$file" "$svc" pim route on --config "$config" >/dev/null 2>&1
}

# assert_ping <file> <from_svc> <target_ip> [desc]
assert_ping() {
    local file="$1" from="$2" target="$3"
    local desc="${4:-ping $from → $target}"
    if in_svc "$file" "$from" ping -c 3 -W 2 "$target" >/dev/null 2>&1; then
        log_ok "$desc"
    else
        log_fail "$desc"
    fi
}

# assert_curl <file> <from_svc> <url> [desc]
assert_curl() {
    local file="$1" from="$2" url="$3"
    local desc="${4:-curl $url from $from}"
    if in_svc "$file" "$from" curl -sf --max-time 15 "$url" >/dev/null 2>&1; then
        log_ok "$desc"
    else
        log_fail "$desc"
    fi
}

# assert_dns <file> <from_svc> <hostname> [desc]
assert_dns() {
    local file="$1" from="$2" host="$3"
    local desc="${4:-DNS resolve $host from $from}"
    if in_svc "$file" "$from" nslookup "$host" >/dev/null 2>&1; then
        log_ok "$desc"
    else
        log_fail "$desc"
    fi
}

# assert_iface_up <file> <svc> <iface_name> [desc]
assert_iface_up() {
    local file="$1" svc="$2" iface="$3"
    local desc="${4:-$iface is UP on $svc}"
    if in_svc "$file" "$svc" ip link show "$iface" 2>/dev/null | grep -q "UP"; then
        log_ok "$desc"
    else
        log_fail "$desc"
    fi
}

# assert_iface_addr <file> <svc> <expected_cidr> [desc]
assert_iface_addr() {
    local file="$1" svc="$2" cidr="$3"
    local desc="${4:-$svc has address $cidr on pim0}"
    if in_svc "$file" "$svc" ip addr show pim0 2>/dev/null | grep -q "$cidr"; then
        log_ok "$desc"
    else
        log_fail "$desc"
    fi
}

# wait_healthy <file> <service> [max_seconds]
wait_healthy() {
    local file="$1" svc="$2" max="${3:-90}"
    local elapsed=0
    log_info "Waiting for $svc to be healthy (up to ${max}s)..."
    while [ $elapsed -lt $max ]; do
        local health
        health=$(docker compose -f "$COMPOSE_DIR/$file" ps "$svc" 2>/dev/null | tail -1 || true)
        if echo "$health" | grep -q "healthy"; then
            return 0
        fi
        sleep 2
        elapsed=$((elapsed+2))
    done
    log_fail "$svc did not become healthy within ${max}s"
    compose "$file" logs "$svc" | tail -30
    return 1
}

# wait_all_healthy <file> [max_seconds]
wait_all_healthy() {
    local file="$1" max="${2:-120}"
    local elapsed=0
    log_info "Waiting for all services in $file to be healthy..."
    while [ $elapsed -lt $max ]; do
        local unhealthy
        unhealthy=$(docker compose -f "$COMPOSE_DIR/$file" ps 2>/dev/null | grep -c "starting\|unhealthy" || true)
        if [ "$unhealthy" -eq 0 ]; then
            log_info "All services healthy"
            return 0
        fi
        sleep 3
        elapsed=$((elapsed+3))
    done
    log_fail "Not all services became healthy within ${max}s"
    compose "$file" ps
    return 1
}

# ── Lifecycle helpers ─────────────────────────────────────────────────────────

# start_stack <file>
start_stack() {
    local file="$1"
    log_info "Starting stack: $file"
    compose "$file" down -v --remove-orphans >/dev/null 2>&1 || true
    compose "$file" up -d --build
}

# stop_stack <file>
stop_stack() {
    local file="$1"
    log_info "Stopping stack: $file"
    compose "$file" down -v --remove-orphans 2>/dev/null || true
}

# dump_logs <file>
dump_logs() {
    local file="$1"
    echo ""
    log_warn "Dumping container logs for $file:"
    compose "$file" logs --no-color 2>/dev/null | tail -100 || true
}

# wait_for_peers <file> <service> [min_peers] [max_seconds]
# Poll pim status --verbose until peer count >= min_peers.
wait_for_peers() {
    local file="$1" svc="$2" min="${3:-1}" max="${4:-60}"
    local elapsed=0
    log_info "Waiting for $svc to have at least $min peer(s) (up to ${max}s)..."
    while [ $elapsed -lt $max ]; do
        local count
        count=$(in_svc "$file" "$svc" pim status --verbose 2>/dev/null \
            | awk -F'[:=]' '/^[[:space:]]*peers[=:]/ {gsub(/[[:space:]]/, "", $2); print $2; exit}')
        count="${count:-0}"
        if [ "${count:-0}" -ge "$min" ]; then
            log_info "$svc has $count peer(s)"
            return 0
        fi
        sleep 2
        elapsed=$((elapsed+2))
    done
    log_fail "$svc did not reach $min peer(s) within ${max}s"
    in_svc "$file" "$svc" pim status --verbose 2>/dev/null || true
    return 1
}

# ── Summary ───────────────────────────────────────────────────────────────────

print_summary() {
    echo ""
    echo -e "${BOLD}────────────────────────────────────────${NC}"
    echo -e "  Results: ${GREEN}$PASS passed${NC}  ${RED}$FAIL failed${NC}  ${YELLOW}$SKIP skipped${NC}"
    echo -e "${BOLD}────────────────────────────────────────${NC}"
    if [ $FAIL -gt 0 ]; then
        return 1
    fi
    return 0
}
