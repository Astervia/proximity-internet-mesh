#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

echo "Connectivity-related files:"
find crates -maxdepth 2 -type f | sort | grep -E 'pim-(core|daemon|discovery|transport|wifidirect)' || true

echo
echo "Key connectivity symbols:"
rg -n \
  "TcpTransport::new|trait Transport|WifiDirectDiscovery|DiscoveryService|initiate_peer_connection|run_wifidirect_consumer|run_discovery_consumer|\\[transport\\]|\\[wifi_direct\\]" \
  crates docs README.md || true
