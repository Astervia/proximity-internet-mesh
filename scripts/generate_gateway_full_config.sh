#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: generate_gateway_full_config.sh [options]

Generate a full PIM gateway config with discovery, Wi-Fi Direct, and Bluetooth
PAN enabled. Interface values are auto-detected when possible and can be
overridden explicitly.

Options:
  -o, --output PATH            Write config to PATH instead of stdout
      --name NAME              Node name to use
      --mesh-ip CIDR           Mesh IP/CIDR for the gateway (default: 10.77.0.1/24)
      --nat-interface IFACE    Internet-facing interface for NAT
      --wifi-interface IFACE   Wi-Fi Direct parent interface
      --bt-interface IFACE     Bluetooth PAN interface (default fallback: bnep0)
      --listen-port PORT       Transport listen port (default: 9100)
      --discovery-port PORT    UDP discovery port (default: 9101)
      --data-dir PATH          Data dir (default: /var/lib/pim)
  -h, --help                   Show this help

Examples:
  scripts/generate_gateway_full_config.sh > /etc/pim/pim.toml
  scripts/generate_gateway_full_config.sh --output /etc/pim/pim.toml
  scripts/generate_gateway_full_config.sh --nat-interface eth1 --wifi-interface wlan1
EOF
}

have_cmd() {
    command -v "$1" >/dev/null 2>&1
}

host_os() {
    if [[ -n "${PIM_HOST_OS:-}" ]]; then
        printf '%s\n' "${PIM_HOST_OS}"
    else
        uname -s
    fi
}

is_macos() {
    [[ "$(host_os)" == "Darwin" ]]
}

first_line() {
    awk 'NF { print; exit }'
}

detect_nat_interface() {
    if have_cmd ip; then
        local route
        route="$(ip route get 1.1.1.1 2>/dev/null | first_line || true)"
        if [[ -n "${route}" ]] && [[ "${route}" =~ [[:space:]]dev[[:space:]]([^[:space:]]+) ]]; then
            printf '%s\n' "${BASH_REMATCH[1]}"
            return 0
        fi

        local def
        def="$(ip route show default 2>/dev/null | first_line || true)"
        if [[ -n "${def}" ]] && [[ "${def}" =~ [[:space:]]dev[[:space:]]([^[:space:]]+) ]]; then
            printf '%s\n' "${BASH_REMATCH[1]}"
            return 0
        fi
    fi

    printf '%s\n' "eth0"
}

detect_wifi_interface() {
    if have_cmd iw; then
        local iface
        iface="$(iw dev 2>/dev/null | awk '$1 == "Interface" { print $2; exit }' || true)"
        if [[ -n "${iface}" ]]; then
            printf '%s\n' "${iface}"
            return 0
        fi
    fi

    if have_cmd ip; then
        local iface
        iface="$(
            ip -o link show 2>/dev/null \
                | awk -F': ' '{print $2}' \
                | sed 's/@.*$//' \
                | grep -E '^(wl|wlan)' \
                | head -n 1 || true
        )"
        if [[ -n "${iface}" ]]; then
            printf '%s\n' "${iface}"
            return 0
        fi
    fi

    printf '%s\n' "wlan0"
}

detect_bluetooth_interface() {
    if have_cmd ip; then
        local iface
        iface="$(
            ip -o link show 2>/dev/null \
                | awk -F': ' '{print $2}' \
                | sed 's/@.*$//' \
                | grep -E '^bnep[0-9]+$' \
                | head -n 1 || true
        )"
        if [[ -n "${iface}" ]]; then
            printf '%s\n' "${iface}"
            return 0
        fi
    fi

    printf '%s\n' "bnep0"
}

shell_quote_comment() {
    printf '%s' "$1" | sed 's/"/\\"/g'
}

output=""
node_name="$(hostname -s 2>/dev/null || printf '%s' gateway-node)"
mesh_ip="10.77.0.1/24"
nat_interface=""
wifi_interface=""
bt_interface=""
listen_port="9100"
discovery_port="9101"
data_dir="/var/lib/pim"

while [[ $# -gt 0 ]]; do
    case "$1" in
        -o|--output)
            output="${2:?missing value for $1}"
            shift 2
            ;;
        --name)
            node_name="${2:?missing value for $1}"
            shift 2
            ;;
        --mesh-ip)
            mesh_ip="${2:?missing value for $1}"
            shift 2
            ;;
        --nat-interface)
            nat_interface="${2:?missing value for $1}"
            shift 2
            ;;
        --wifi-interface)
            wifi_interface="${2:?missing value for $1}"
            shift 2
            ;;
        --bt-interface)
            bt_interface="${2:?missing value for $1}"
            shift 2
            ;;
        --listen-port)
            listen_port="${2:?missing value for $1}"
            shift 2
            ;;
        --discovery-port)
            discovery_port="${2:?missing value for $1}"
            shift 2
            ;;
        --data-dir)
            data_dir="${2:?missing value for $1}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            printf 'Unknown argument: %s\n\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if is_macos; then
    cat >&2 <<'EOF'
generate_gateway_full_config.sh does not support macOS because gateway mode is currently Linux-only.
Use `pim config generate client` or `pim config generate relay` on macOS, or run this script on Linux for gateway configs.
EOF
    exit 1
fi

if [[ -z "${nat_interface}" ]]; then
    nat_interface="$(detect_nat_interface)"
fi
if [[ -z "${wifi_interface}" ]]; then
    wifi_interface="$(detect_wifi_interface)"
fi
if [[ -z "${bt_interface}" ]]; then
    bt_interface="$(detect_bluetooth_interface)"
fi

generated_at="$(date -u +'%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || printf '%s' unknown)"
detect_note_nat="$(shell_quote_comment "${nat_interface}")"
detect_note_wifi="$(shell_quote_comment "${wifi_interface}")"
detect_note_bt="$(shell_quote_comment "${bt_interface}")"

config="$(cat <<EOF
# Generated by scripts/generate_gateway_full_config.sh at ${generated_at}
# Auto-detected interfaces:
#   gateway.nat_interface  = "${detect_note_nat}"
#   wifi_direct.interface  = "${detect_note_wifi}"
#   bluetooth.interface    = "${detect_note_bt}"
#
# Review these values before starting the daemon on a real host:
#   ip route get 1.1.1.1
#   iw dev
#   ip -br link
#   bluetoothctl show

[node]
name = "${node_name}"
data_dir = "${data_dir}"

[interface]
name = "pim0"
mesh_ip = "${mesh_ip}"
mtu = 1400

[discovery]
enabled = true
port = ${discovery_port}
broadcast_interval_ms = 5000
peer_timeout_ms = 30000
connect_relays = true
connect_gateways = true

[transport]
type = "tcp"
listen_port = ${listen_port}

[routing]
max_hops = 10
algorithm = "distance-vector"
route_expiry_s = 300

[gateway]
enabled = true
nat_interface = "${nat_interface}"
max_connections = 200

[relay]
enabled = true

[security]
key_file = "${data_dir}/node.key"
require_encryption = true

[wifi_direct]
enabled = true
interface = "${wifi_interface}"
go_intent = 12
listen_channel = 6
op_channel = 6
connect_method = "pbc"

[bluetooth]
enabled = true
interface = "${bt_interface}"
radio_discovery_enabled = true
device_name_prefix = "PIM-"
local_alias = "PIM-${node_name}"
auto_discover_peers = true
poll_interval_ms = 2000
scan_interval_ms = 5000
peer_discovery_interval_ms = 2000
bluetoothctl_timeout_s = 15
discoverable_timeout_s = 180
startup_timeout_ms = 15000

# Optional static peers. Leave these commented if you want to rely entirely on
# auto-discovery and inbound connections.
#
# [[peers]]
# mechanism = "tcp"
# address = "192.168.1.20:${listen_port}"
# label = "relay-a"
#
# [[peers]]
# mechanism = "bluetooth"
# ip = "192.168.44.2"
# label = "bt-relay-a"
EOF
)"

if [[ -n "${output}" ]]; then
    printf '%s\n' "${config}" > "${output}"
else
    printf '%s\n' "${config}"
fi
