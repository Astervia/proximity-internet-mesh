#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "expected to find: $expected" >&2
        echo "--- file: $file ---" >&2
        cat "$file" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq "$unexpected" "$file"; then
        echo "did not expect to find: $unexpected" >&2
        echo "--- file: $file ---" >&2
        cat "$file" >&2
        exit 1
    fi
}

linux_client="$tmpdir/linux-client.toml"
PIM_HOST_OS=Linux scripts/generate_client_full_config.sh > "$linux_client"
assert_contains "$linux_client" 'name = "pim0"'
assert_contains "$linux_client" '[wifi_direct]'
assert_contains "$linux_client" 'enabled = true'
assert_contains "$linux_client" 'interface = "wlan0"'
assert_contains "$linux_client" 'interface = "auto"'
assert_contains "$linux_client" 'connect_pan = true'
assert_contains "$linux_client" 'serve_nap = false'
assert_contains "$linux_client" 'nap_bridge = "br-bt"'
assert_contains "$linux_client" 'request_dhcp = true'
assert_contains "$linux_client" 'dhcp_enabled = false'

mac_client="$tmpdir/macos-client.toml"
PIM_HOST_OS=Darwin scripts/generate_client_full_config.sh > "$mac_client"
assert_contains "$mac_client" 'name = "utun0"'
assert_contains "$mac_client" '# Wi-Fi Direct is currently Linux-only; leave this disabled on macOS.'
assert_contains "$mac_client" '# macOS Bluetooth PAN uses the host Bluetooth stack. Install blueutil for radio discovery/pair/connect.'
assert_contains "$mac_client" '[bluetooth]'
assert_contains "$mac_client" 'enabled = true'
assert_contains "$mac_client" 'interface = "en0"'
assert_contains "$mac_client" 'interface = "bridge0"'
assert_contains "$mac_client" 'serve_nap = false'
assert_not_contains "$mac_client" '#   ip -br link'
assert_not_contains "$mac_client" '#   iw dev'
assert_not_contains "$mac_client" '#   bluetoothctl show'

linux_gateway="$tmpdir/linux-gateway.toml"
PIM_HOST_OS=Linux scripts/generate_gateway_full_config.sh > "$linux_gateway"
assert_contains "$linux_gateway" 'serve_nap = true'
assert_contains "$linux_gateway" 'nap_bridge = "br-bt"'
assert_contains "$linux_gateway" 'nap_bridge_addr = "192.168.44.1/24"'
assert_contains "$linux_gateway" 'dhcp_enabled = true'
assert_contains "$linux_gateway" 'request_dhcp = false'

mac_gateway="$tmpdir/macos-gateway.toml"
PIM_HOST_OS=Darwin scripts/generate_gateway_full_config.sh > "$mac_gateway"
assert_contains "$mac_gateway" 'name = "utun0"'
assert_contains "$mac_gateway" 'nat_interface = "en0"'
assert_contains "$mac_gateway" '# Wi-Fi Direct is currently Linux-only; leave this disabled on macOS.'
assert_contains "$mac_gateway" '# macOS Bluetooth PAN uses the host Bluetooth stack. Install blueutil for radio discovery/pair/connect.'
assert_contains "$mac_gateway" '[gateway]'
assert_contains "$mac_gateway" 'enabled = true'
assert_contains "$mac_gateway" 'connect_pan = true'
assert_contains "$mac_gateway" 'serve_nap = false'
assert_not_contains "$mac_gateway" '#   ip route get 1.1.1.1'
assert_not_contains "$mac_gateway" '#   iw dev'
assert_not_contains "$mac_gateway" '#   ip -br link'

echo "config generator checks passed"
