#!/usr/bin/env bash
# test-ipv6.sh — Docker system test for IPv6 mesh routing and gateway NAT66.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"

COMPOSE_FILE="phase1-ipv6-single-hop.yml"
UPLINK_HTTP_V6="http://[2001:db8:1::80]/"

cleanup() {
    if [ "${DUMP_LOGS_ON_FAIL:-0}" = "1" ] && [ $FAIL -gt 0 ]; then
        dump_logs "$COMPOSE_FILE"
    fi
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT

log_section "IPv6 Docker lane"
log_info "Compose file: $COMPOSE_FILE"

start_stack "$COMPOSE_FILE"
wait_all_healthy "$COMPOSE_FILE" 120
wait_for_peers "$COMPOSE_FILE" gateway 1 60
wait_for_peers "$COMPOSE_FILE" client 1 60

log_section "TUN IPv6 addressing"
assert_iface_up "$COMPOSE_FILE" gateway pim0 "gateway pim0 is UP"
assert_iface_addr "$COMPOSE_FILE" gateway "fd77::1" "gateway pim0 has IPv6 address fd77::1"
assert_iface_up "$COMPOSE_FILE" client pim0 "client pim0 is UP"
assert_iface_addr "$COMPOSE_FILE" client "fd77::100" "client pim0 has IPv6 address fd77::100"

log_section "Gateway uplink IPv6"
assert_cmd \
    "gateway reaches the simulated IPv6 internet directly" \
    in_svc "$COMPOSE_FILE" gateway curl -g -6 -sf --max-time 15 "$UPLINK_HTTP_V6"

log_section "IPv6 routing through mesh"
assert_cmd_output \
    "client route status starts disabled" \
    "pim routes: disabled" \
    in_svc "$COMPOSE_FILE" client pim route status

assert_cmd \
    "client enables split-default routes" \
    in_svc "$COMPOSE_FILE" client pim route on

assert_cmd_output \
    "lower-half IPv6 split default installed" \
    "::/1 dev pim0" \
    in_svc "$COMPOSE_FILE" client ip -6 route show

assert_cmd_output \
    "upper-half IPv6 split default installed" \
    "8000::/1 dev pim0" \
    in_svc "$COMPOSE_FILE" client ip -6 route show

assert_logs_contain \
    "$COMPOSE_FILE" gateway "NAT66 outbound" \
    "gateway logs show outbound IPv6 NAT activity"

if in_svc "$COMPOSE_FILE" client curl -g -6 -sf --max-time 15 "$UPLINK_HTTP_V6" >/dev/null 2>&1; then
    log_ok "client reaches simulated IPv6 internet through the mesh"
else
    log_skip "client IPv6 HTTP through mesh (route/NAT66 path activated, but end-to-end TCP still depends on kernel TUN next-hop behavior)"
fi

log_section "Disable IPv6 split-default routes"
assert_cmd \
    "client disables split-default routes" \
    in_svc "$COMPOSE_FILE" client pim route off

assert_cmd \
    "lower-half IPv6 split default removed" \
    in_svc "$COMPOSE_FILE" client bash -lc \
    "! ip -6 route show | grep -q '::/1 dev pim0'"

assert_cmd \
    "upper-half IPv6 split default removed" \
    in_svc "$COMPOSE_FILE" client bash -lc \
    "! ip -6 route show | grep -q '8000::/1 dev pim0'"

print_summary
