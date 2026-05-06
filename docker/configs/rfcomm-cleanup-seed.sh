#!/bin/sh
# Pre-daemon init for the rfcomm-cleanup lab. Mounts the fake
# bluetoothctl into /tmp/fake-bin and seeds a single stale row in
# peers.db (`first_paired_at_s` = now - 2 days) so the very first
# cleanup tick passes the unreachable-lifetime threshold.
#
# Sourced from the compose entrypoint; falls through to /entrypoint.sh.
# `/bin/sh` on Debian is dash — no `pipefail`, so be deliberate about
# error handling instead.

set -eu

mkdir -p /tmp/fake-bin /var/lib/pim
cp /etc/pim/lab/rfcomm-cleanup-bluetoothctl.sh /tmp/fake-bin/bluetoothctl
chmod +x /tmp/fake-bin/bluetoothctl

STALE_TS=$(( $(date +%s) - 172800 ))
sqlite3 /var/lib/pim/peers.db <<SQL
CREATE TABLE IF NOT EXISTS rfcomm_peer_lifecycle (
    bd_addr             TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    first_paired_at_s   INTEGER NOT NULL,
    last_connected_at_s INTEGER
);
INSERT INTO rfcomm_peer_lifecycle
    (bd_addr, name, first_paired_at_s, last_connected_at_s)
    VALUES ('AA:BB:CC:DD:EE:FF', 'PIM-test-stale', $STALE_TS, NULL);
SQL
chmod 0600 /var/lib/pim/peers.db

exec /entrypoint.sh
