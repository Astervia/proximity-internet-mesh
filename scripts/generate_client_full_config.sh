#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: generate_client_full_config.sh [options]

Generate a full PIM client config with discovery plus the host's supported
peer-link backends enabled. Interface values are auto-detected when possible
and can be overridden explicitly.

Options:
  -o, --output PATH            Write config to PATH instead of stdout
      --name NAME              Node name to use
      --mesh-ip CIDR|auto      Mesh IP/CIDR for the client (default: auto)
      --wifi-interface IFACE   Wi-Fi Direct parent interface
      --bt-interface IFACE     Preferred Bluetooth PAN interface (default: auto)
      --listen-port PORT       Transport listen port (default: 9100)
      --discovery-port PORT    UDP discovery port (default: 9101)
      --data-dir PATH          Data dir (default: /var/lib/pim)
  -h, --help                   Show this help

Examples:
  scripts/generate_client_full_config.sh > /etc/pim/pim.toml
  scripts/generate_client_full_config.sh --output /etc/pim/pim.toml
  scripts/generate_client_full_config.sh --wifi-interface wlan1 --bt-interface bnep1
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

detect_wifi_interface() {
    if is_macos; then
        return 1
    fi

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
    if is_macos; then
        return 1
    fi

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

    printf '%s\n' "auto"
}

shell_quote_comment() {
    printf '%s' "$1" | sed 's/"/\\"/g'
}

output=""
node_name="$(hostname -s 2>/dev/null || printf '%s' client-node)"
mesh_ip="auto"
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

if [[ -z "${wifi_interface}" ]]; then
    wifi_interface="$(detect_wifi_interface || true)"
fi
if [[ -z "${bt_interface}" ]]; then
    bt_interface="$(detect_bluetooth_interface || true)"
fi

generated_at="$(date -u +'%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || printf '%s' unknown)"
detect_note_wifi="$(shell_quote_comment "${wifi_interface}")"
detect_note_bt="$(shell_quote_comment "${bt_interface}")"
interface_name="pim0"
review_commands=$'#   ip -br link\n#   iw dev\n#   bluetoothctl show'
wifi_enabled="true"
bt_enabled="true"
wifi_mac_comment=""
bt_mac_comment=""

if is_macos; then
    interface_name="utun0"
    review_commands=$'#   ifconfig -l\n#   blueutil --power\n#   arp -an -i bridge0'
    if [[ -z "${wifi_interface}" ]]; then
        wifi_interface="en0"
    fi
    if [[ -z "${bt_interface}" ]]; then
        bt_interface="bridge0"
    fi
    detect_note_wifi="$(shell_quote_comment "${wifi_interface}")"
    detect_note_bt="$(shell_quote_comment "${bt_interface}")"
    wifi_enabled="false"
    bt_enabled="true"
    wifi_mac_comment="# Wi-Fi Direct is currently Linux-only; leave this disabled on macOS."
    bt_mac_comment="# macOS Bluetooth PAN uses the host Bluetooth stack. Install blueutil for radio discovery/pair/connect."
fi

config="$(cat <<EOF
# Generated by scripts/generate_client_full_config.sh at ${generated_at}
# Auto-detected interfaces:
#   wifi_direct.interface  = "${detect_note_wifi}"
#   bluetooth.interface    = "${detect_note_bt}"
#
# Review these values before starting the daemon on a real host:
${review_commands}

[node]
name = "${node_name}"
data_dir = "${data_dir}"

[interface]
name = "${interface_name}"
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

[relay]
enabled = false

[security]
key_file = "${data_dir}/node.key"
require_encryption = true

[wifi_direct]
${wifi_mac_comment}
enabled = ${wifi_enabled}
interface = "${wifi_interface}"
go_intent = 7
listen_channel = 6
op_channel = 6
connect_method = "pbc"

[bluetooth]
${bt_mac_comment}
enabled = ${bt_enabled}
interface = "${bt_interface}"
radio_discovery_enabled = true
device_name_prefix = "PIM-"
local_alias = "PIM-${node_name}"
connect_pan = true
serve_nap = false
nap_bridge = "br-bt"
auto_discover_peers = true
poll_interval_ms = 2000
scan_interval_ms = 5000
peer_discovery_interval_ms = 2000
bluetoothctl_timeout_s = 15
discoverable_timeout_s = 180
startup_timeout_ms = 15000

# Optional static peers. Leave these commented if you want to rely entirely on
# auto-discovery and outbound connection attempts.
#
# [[peers]]
# mechanism = "tcp"
# address = "192.168.1.20:${listen_port}"
# label = "relay-a"
#
# [[peers]]
# mechanism = "bluetooth"
# ip = "192.168.44.2"
# label = "bt-gateway-a"
EOF
)"

if [[ -n "${output}" ]]; then
    printf '%s\n' "${config}" > "${output}"
else
    printf '%s\n' "${config}"
fi
