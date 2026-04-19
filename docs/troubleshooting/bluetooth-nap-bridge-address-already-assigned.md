# Bluetooth NAP Bridge Address Already Assigned

Symptom:

```text
WARN pim_daemon: Bluetooth PAN watcher exited with error: ip failed: failed to assign address 192.168.44.1/24 to br-bt: Error: ipv4: Address already assigned.
```

This usually means the Linux bridge named in `[bluetooth].nap_bridge` already exists and already has the configured `[bluetooth].nap_bridge_addr` on it. In practice that is most often:

- a previous `pim-daemon` run that did not clean up `br-bt`
- another local process already managing the same bridge
- a manual bridge setup that matches the daemon config

## Quick Check

Verify whether `br-bt` is already in the expected state:

```bash
ip -br link show br-bt
ip -4 addr show dev br-bt
bridge link show master br-bt
```

If `br-bt` is up and already shows `192.168.44.1/24`, the failure is an idempotency problem rather than a missing-resource problem.

## Safe Recovery

If no other service should own `br-bt`, remove the stale bridge and restart the daemon:

```bash
sudo pkill -TERM -x pim-daemon
sudo ip link set br-bt down 2>/dev/null || true
sudo ip link delete br-bt 2>/dev/null || true
sudo pim up --config /etc/pim/pim.toml
```

If the daemon had installed Bluetooth gateway NAT rules previously, review the cleanup steps in [../operations/bluetooth-gateway-shutdown.md](../operations/bluetooth-gateway-shutdown.md) before restarting.

## If The Bridge Should Be Reused

If you intentionally created `br-bt` yourself and it already has the correct address, make sure only one owner is managing it. Do not run a second service that tries to recreate the bridge with the same subnet.

Check for competing processes:

```bash
pgrep -af 'pim-daemon|bt-network|dnsmasq'
```

Also confirm the configured values match the live bridge:

```bash
grep -nE 'nap_bridge|nap_bridge_addr|serve_nap' /etc/pim/pim.toml
ip -4 addr show dev br-bt
```

## If The Address Is Wrong

If `br-bt` exists but has the wrong subnet or address, remove it and let `pim-daemon` recreate it from config:

```bash
sudo ip link set br-bt down
sudo ip link delete br-bt
sudo pim up --config /etc/pim/pim.toml
```

## Prevention

- Prefer stopping the gateway with `SIGTERM` or `sudo pim down`, not `SIGKILL`
- Avoid running multiple Bluetooth gateway configs that all declare `nap_bridge = "br-bt"`
- Keep one authoritative `nap_bridge_addr` per host
- Use the Bluetooth gateway shutdown procedure when a previous run may have left bridge or NAT state behind
