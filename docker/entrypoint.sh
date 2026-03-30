#!/bin/bash
# Container entrypoint for pim-daemon.
# Environment variables:
#   PIM_CONFIG   — path to the TOML config  (default: /etc/pim/pim.toml)
#   PIM_PID_FILE — path to the PID file     (default: /run/pim.pid)
#   RUST_LOG     — tracing filter            (default: info)

set -euo pipefail

CONFIG="${PIM_CONFIG:-/etc/pim/pim.toml}"
PID_FILE="${PIM_PID_FILE:-/run/pim.pid}"
export RUST_LOG="${RUST_LOG:-info}"

# Ensure runtime directories exist (images are read-only on some runtimes).
mkdir -p /var/lib/pim /run

if [ ! -f "$CONFIG" ]; then
    echo "[entrypoint] ERROR: config not found at $CONFIG" >&2
    exit 1
fi

echo "[entrypoint] node=$(grep -oP '(?<=^name = ").*(?=")' "$CONFIG" | head -1)"
echo "[entrypoint] config=$CONFIG"
echo "[entrypoint] starting pim-daemon"

exec pim-daemon "$CONFIG" "$PID_FILE"
