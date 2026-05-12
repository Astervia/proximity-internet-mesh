use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::NodeRole;

pub(crate) fn cmd_config_generate(
    roles: Vec<NodeRole>,
    name: Option<String>,
    output: Option<PathBuf>,
    force: bool,
) -> Result<()> {
    let rendered = render_config_template(&roles, name.as_deref());

    if let Some(path) = output {
        let mut options = std::fs::OpenOptions::new();
        options.write(true);
        if force {
            options.create(true).truncate(true);
        } else {
            options.create_new(true);
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        use std::io::Write;
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && !force => {
                bail!(
                    "refusing to overwrite existing file: {} (use --force to overwrite)",
                    path.display()
                );
            }
            Err(e) => {
                return Err(anyhow::Error::new(e)
                    .context(format!("failed to open config file {}", path.display())));
            }
        };

        file.write_all(rendered.as_bytes())
            .with_context(|| format!("failed to write config template to {}", path.display()))?;

        println!("Wrote config template to {}", path.display());
    } else {
        print!("{rendered}");
    }

    Ok(())
}

// ── Template renderer ────────────────────────────────────────────────────────
//
// The output is a complete, parseable `pim.toml` whose layout mirrors
// `pim-core/src/config/model.rs` 1:1 — every field defined on `Config`
// (and its sub-structs) appears here, either with its real value or as
// a commented `# key = …` line with an inline explanation. Tests assert
// the rendered string round-trips through `Config::from_toml_str`.
//
// Conventions:
//   - Active values are written verbatim.
//   - Optional fields (`Option<T>` in the model — `mesh_ipv6`,
//     `dhcp_range`, `dhcp_dns`) are emitted commented-out with a
//     one-line explanation of how to enable them.
//   - Sections that are role-conditional (`[gateway]`) are emitted as
//     a commented placeholder when the role is absent — preserving the
//     existing CLI ergonomic where users uncomment-to-enable.
//   - `[bluetooth]`, `[bluetooth_rfcomm]`, and `[wifi_direct]` are emitted as full blocks with
//     `enabled = false` (matching `BluetoothConfig::default()` /
//     `WifiDirectConfig::default()` in pim-core), so all knobs are
//     visible and users edit a single toggle to turn them on.

pub(crate) fn render_config_template(roles: &[NodeRole], override_name: Option<&str>) -> String {
    let roles = unique_roles(roles);
    let is_gateway = roles.contains(&NodeRole::Gateway);
    let is_relay = roles.contains(&NodeRole::Relay);
    let is_client = roles.contains(&NodeRole::Client);
    let node_name = override_name
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_node_name(&roles));
    let peer_example = default_peer_example(&roles);
    let roles_label = roles
        .iter()
        .map(|role| role.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    let mut out = String::with_capacity(8 * 1024);

    push_header(&mut out, &roles_label);
    push_node(&mut out, &node_name);
    push_interface(&mut out);
    push_discovery(&mut out);
    push_mesh(&mut out);
    push_transport(&mut out);
    push_routing(&mut out);
    push_gateway(&mut out, is_gateway);
    push_relay(&mut out, is_relay, is_gateway);
    push_security(&mut out);
    push_bluetooth(&mut out, &node_name);
    push_bluetooth_rfcomm(&mut out);
    push_bluetooth_coc(&mut out);
    push_wifi_direct(&mut out);
    push_static_peers(&mut out, peer_example, is_client, is_relay, is_gateway);

    out
}

// ── Section writers ──────────────────────────────────────────────────────────

fn push_header(out: &mut String, roles_label: &str) {
    push_line(out, "# Proximity Internet Mesh configuration template");
    push_line(out, &format!("# Roles enabled: {roles_label}"));
    push_line(
        out,
        "# Edit the values below, save the file, then start the daemon with:",
    );
    push_line(out, "#   sudo pim up --config /etc/pim/pim.toml");
    push_blank(out);
}

fn push_node(out: &mut String, node_name: &str) {
    push_line(out, "[node]");
    push_line(
        out,
        "# Human-readable node name shown in logs, status output, and to other peers.",
    );
    push_line(out, &format!("name = {:?}", node_name));
    push_line(
        out,
        "# Writable directory for the Ed25519 node key, the trust store, and runtime state.",
    );
    push_line(out, "data_dir = \"/var/lib/pim\"");
    push_blank(out);
}

fn push_interface(out: &mut String) {
    push_line(out, "[interface]");
    push_line(
        out,
        "# TUN interface exposed by the daemon. Linux commonly uses pim0; macOS must use a utunN name.",
    );
    push_line(out, &format!("name = {:?}", default_interface_name()));
    push_line(
        out,
        "# Mesh IPv4 prefix. Each node's host bits are derived from its NodeId, so two daemons",
    );
    push_line(
        out,
        "# sharing this prefix get unique addresses without coordination. Widen to /14 if the",
    );
    push_line(out, "# default /16 birthday-collides on small meshes.");
    push_line(
        out,
        &format!(
            "mesh_ipv4_prefix = {:?}",
            pim_core::DEFAULT_MESH_IPV4_PREFIX
        ),
    );
    push_line(
        out,
        "# Mesh IPv6 prefix. /64 is collision-free at PIM scale; the default ULA is recommended.",
    );
    push_line(
        out,
        &format!(
            "mesh_ipv6_prefix = {:?}",
            pim_core::DEFAULT_MESH_IPV6_PREFIX
        ),
    );
    push_line(
        out,
        "# Keep this aligned with the mesh MTU expected by other peers.",
    );
    push_line(out, "mtu = 1400");
    push_blank(out);
}

fn push_discovery(out: &mut String) {
    push_line(out, "[discovery]");
    push_line(
        out,
        "# UDP-broadcast peer discovery (`PIMD` advertisements on `port`). Limited to a single",
    );
    push_line(
        out,
        "# broadcast domain — cross-subnet topologies need static [[peers]] entries.",
    );
    push_line(
        out,
        "# Set `enabled = false` to rely entirely on static peers below.",
    );
    push_line(out, "enabled = true");
    push_line(
        out,
        "# UDP port used for sending and receiving discovery broadcasts.",
    );
    push_line(out, "port = 9101");
    push_line(
        out,
        "# How often this node broadcasts its presence (milliseconds).",
    );
    push_line(out, "broadcast_interval_ms = 5000");
    push_line(
        out,
        "# How long a previously-seen peer remains in the table before expiry (milliseconds).",
    );
    push_line(out, "peer_timeout_ms = 30000");
    push_line(
        out,
        "# Auto-connect to discovered peers advertising relay capability.",
    );
    push_line(out, "connect_relays = true");
    push_line(
        out,
        "# Auto-connect to discovered peers advertising gateway capability.",
    );
    push_line(out, "connect_gateways = true");
    push_blank(out);
}

fn push_mesh(out: &mut String) {
    push_line(out, "[mesh]");
    push_line(
        out,
        "# Private-mesh membership (optional). Absent or `mode = \"open\"` means any peer",
    );
    push_line(
        out,
        "# can connect — this is the default. `mode = \"private\"` requires every node in",
    );
    push_line(
        out,
        "# the mesh to know the same `passphrase`; non-members can't decrypt discovery",
    );
    push_line(
        out,
        "# broadcasts and can't complete the encrypted handshake.",
    );
    push_line(out, "mode = \"open\"");
    push_line(
        out,
        "# Required when mode = \"private\". UTF-8 string, stretched via Argon2id at startup.",
    );
    push_line(out, "# passphrase = \"correct horse battery staple\"");
    push_line(
        out,
        "# Optional cosmetic label. Mixed into the KDF salt, so two meshes that share a",
    );
    push_line(
        out,
        "# passphrase but use different mesh_ids derive distinct secrets and won't",
    );
    push_line(
        out,
        "# interconnect. Renaming mesh_id invalidates existing peer sessions.",
    );
    push_line(out, "# mesh_id = \"office\"");
    push_line(
        out,
        "# Argon2id parameters. Defaults target ~100 ms on a desktop; tune down for embedded.",
    );
    push_line(out, "# [mesh.kdf]");
    push_line(out, "# m_cost_kib = 65536");
    push_line(out, "# t_cost = 3");
    push_line(out, "# p_cost = 1");
    push_blank(out);
}

fn push_transport(out: &mut String) {
    push_line(out, "[transport]");
    push_line(
        out,
        "# Wire transport backend. Currently \"tcp\" is the only supported value.",
    );
    push_line(out, "type = \"tcp\"");
    push_line(
        out,
        "# TCP port this node listens on for direct peer sessions.",
    );
    push_line(out, "listen_port = 9100");
    push_line(
        out,
        "# Maximum reconnect attempts per peer before giving up.",
    );
    push_line(out, "max_reconnect_attempts = 20");
    push_line(
        out,
        "# Timeout for outbound TCP connect attempts (milliseconds).",
    );
    push_line(out, "connect_timeout_ms = 3000");
    push_blank(out);
}

fn push_routing(out: &mut String) {
    push_line(out, "[routing]");
    push_line(
        out,
        "# Distance-vector settings used for route propagation and expiry.",
    );
    push_line(
        out,
        "# `max_hops` bounds how far a route advertisement can travel.",
    );
    push_line(out, "max_hops = 10");
    push_line(out, "algorithm = \"distance-vector\"");
    push_line(
        out,
        "# How long learned routes survive before being re-advertised (seconds).",
    );
    push_line(out, "route_expiry_s = 300");
    push_line(
        out,
        "# DNS resolvers handed to the system resolver (`resolvectl dns pim0 \u{2026}`)",
    );
    push_line(
        out,
        "# while split-default routing is engaged. Without this list the resolver",
    );
    push_line(
        out,
        "# keeps its DHCP-provided upstream, which becomes unreachable the moment",
    );
    push_line(
        out,
        "# the local uplink (wifi / wired) goes down — name resolution then fails",
    );
    push_line(
        out,
        "# even though the IP path through the mesh is live. Empty list = skip DNS",
    );
    push_line(
        out,
        "# management entirely (corporate VPN tooling / NetworkManager dispatchers /",
    );
    push_line(
        out,
        "# hand-managed /etc/resolv.conf own the resolver instead).",
    );
    push_line(out, "dns_servers = [\"1.1.1.1\", \"1.0.0.1\", \"8.8.8.8\"]");
    push_blank(out);
}

fn push_gateway(out: &mut String, is_gateway: bool) {
    if is_gateway {
        push_line(out, "[gateway]");
        push_line(
            out,
            "# Enable NAT and internet egress on a node with upstream connectivity.",
        );
        push_line(
            out,
            "# A gateway is implicitly also a relay and a client (capability bits 0x07).",
        );
        push_line(out, "enabled = true");
        push_line(
            out,
            "# Replace this with the real internet-facing interface on the host.",
        );
        push_line(
            out,
            &format!("nat_interface = {:?}", default_gateway_nat_interface()),
        );
        push_line(
            out,
            "# Maximum concurrent gateway connection-tracking entries.",
        );
        push_line(out, "max_connections = 200");
    } else {
        push_line(
            out,
            "# [gateway]  # Uncomment this section only on a node that should provide internet access.",
        );
        push_line(out, "# enabled = true");
        push_line(
            out,
            &format!("# nat_interface = {:?}", default_gateway_nat_interface()),
        );
        push_line(out, "# max_connections = 200");
    }
    push_blank(out);
}

fn push_relay(out: &mut String, is_relay: bool, is_gateway: bool) {
    push_line(out, "[relay]");
    push_line(
        out,
        "# Relay nodes forward mesh frames for other peers in addition to originating their",
    );
    push_line(
        out,
        "# own (capability bits 0x03 = relay + client). Set false to run as client-only (0x01)",
    );
    push_line(
        out,
        "# — other nodes will not initiate connections to a client-only peer.",
    );
    if is_gateway {
        push_line(
            out,
            "# Note: a gateway is implicitly also a relay regardless of this setting.",
        );
    }
    push_line(out, &format!("enabled = {}", is_relay));
    push_blank(out);
}

fn push_security(out: &mut String) {
    push_line(out, "[security]");
    push_line(
        out,
        "# The daemon creates this Ed25519 private key on first startup if it does not exist.",
    );
    push_line(out, "key_file = \"/var/lib/pim/node.key\"");
    push_line(
        out,
        "# Reject direct peer sessions that do not complete the authenticated handshake.",
    );
    push_line(out, "require_encryption = true");
    push_line(
        out,
        "# Authorization policy applied AFTER peer identity is authenticated:",
    );
    push_line(
        out,
        "#   allow_all          — admit any authenticated peer (default)",
    );
    push_line(
        out,
        "#   allow_list         — admit only NodeIds listed in `authorized_peers`",
    );
    push_line(
        out,
        "#   trust_on_first_use — admit on first contact and persist identity to trust_store_file",
    );
    push_line(out, "authorization_policy = \"allow_all\"");
    push_line(
        out,
        "# Used only when authorization_policy = \"allow_list\". 64-hex-char NodeIds.",
    );
    push_line(
        out,
        "# authorized_peers = [\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"]",
    );
    push_line(
        out,
        "# Used by trust_on_first_use to remember peers that handshake successfully.",
    );
    push_line(
        out,
        "trust_store_file = \"/var/lib/pim/trusted-peers.toml\"",
    );
    push_blank(out);
}

fn push_bluetooth(out: &mut String, node_name: &str) {
    let bt_iface = default_bluetooth_interface();
    let bt_iface_comment = if cfg!(target_os = "macos") {
        "macOS — host Bluetooth stack PAN bridge"
    } else {
        "Linux — \"auto\" prefers a configured iface, falls back to live bnep* / enx*"
    };

    push_line(out, "[bluetooth]");
    push_line(
        out,
        "# Bluetooth PAN peer link-establishment — macOS host stack and Linux BlueZ.",
    );
    push_line(
        out,
        "# This mechanism finds peers and learns their PAN IPs; the existing TCP transport",
    );
    push_line(
        out,
        "# then connects to that IP for the encrypted handshake. NOT a separate transport.",
    );
    push_line(
        out,
        "# Off by default — flip `enabled = true` once BlueZ / blueutil are installed.",
    );
    push_line(out, "enabled = false");
    push_line(out, &format!("# {bt_iface_comment}."));
    push_line(out, &format!("interface = {:?}", bt_iface));
    push_line(
        out,
        "# Radio-level scanning for new peers via bluetoothctl/blueutil.",
    );
    push_line(out, "radio_discovery_enabled = true");
    push_line(
        out,
        "# Filter inquiry results by Bluetooth device-name prefix.",
    );
    push_line(out, "device_name_prefix = \"PIM-\"");
    push_line(
        out,
        "# Local Bluetooth controller alias broadcast to nearby devices. Empty derives from node.name.",
    );
    push_line(out, &format!("local_alias = \"PIM-{node_name}\""));
    push_line(
        out,
        "# Allow outbound PAN/NAP connection attempts to discovered peers.",
    );
    push_line(out, "connect_pan = true");
    push_line(
        out,
        "# Linux NAP server: serve a local NAP on `nap_bridge` and supervise dnsmasq DHCP.",
    );
    push_line(
        out,
        "# Off by default — flip to true on a Linux gateway acting as the BT access point.",
    );
    push_line(out, "serve_nap = false");
    push_line(out, "nap_bridge = \"br-bt\"");
    push_line(
        out,
        "# IPv4 CIDR assigned to nap_bridge when the daemon manages it (Linux NAP server only).",
    );
    push_line(out, "nap_bridge_addr = \"192.168.44.1/24\"");
    push_line(
        out,
        "# Daemon-supervised dnsmasq DHCP on the bridge (Linux NAP server only).",
    );
    push_line(out, "dhcp_enabled = true");
    push_line(
        out,
        "# Optional explicit DHCP pool. When unset, derived from `nap_bridge_addr`.",
    );
    push_line(out, "# dhcp_range = \"192.168.44.10,192.168.44.200\"");
    push_line(out, "dhcp_lease_time = \"12h\"");
    push_line(
        out,
        "# Optional DNS list advertised to DHCP clients; otherwise inherits /etc/resolv.conf.",
    );
    push_line(out, "# dhcp_dns = \"1.1.1.1,8.8.8.8\"");
    push_line(
        out,
        "# Linux PAN client side: request DHCP on the resolved PAN interface after pairing.",
    );
    push_line(out, "request_dhcp = true");
    push_line(
        out,
        "# Read peer IPs from the PAN interface neighbor table (ip neigh / arp).",
    );
    push_line(out, "auto_discover_peers = true");
    push_line(
        out,
        "# Polling cadence while waiting for the PAN interface to come up (ms).",
    );
    push_line(out, "poll_interval_ms = 2000");
    push_line(out, "# Radio-level inquiry cadence (ms).");
    push_line(out, "scan_interval_ms = 5000");
    push_line(
        out,
        "# Polling cadence for neighbor-table peer discovery once the interface is up (ms).",
    );
    push_line(out, "peer_discovery_interval_ms = 2000");
    push_line(out, "# bluetoothctl operation timeout (seconds).");
    push_line(out, "bluetoothctl_timeout_s = 15");
    push_line(
        out,
        "# How long the controller stays discoverable after startup (seconds).",
    );
    push_line(out, "discoverable_timeout_s = 180");
    push_line(
        out,
        "# Maximum time to wait for the PAN interface to appear before giving up (ms).",
    );
    push_line(out, "startup_timeout_ms = 15000");
    push_blank(out);
}

fn push_bluetooth_rfcomm(out: &mut String) {
    push_line(out, "[bluetooth_rfcomm]");
    push_line(
        out,
        "# Bluetooth RFCOMM direct-channel discovery — Linux daemon, macOS sidecar.",
    );
    push_line(
        out,
        "# Independent from PAN/NAP: paired devices are dialed over RFCOMM, then bridged",
    );
    push_line(
        out,
        "# to the local TCP listener so normal PIM handshakes and sessions are reused.",
    );
    push_line(
        out,
        "# Default now `false` — L2CAP CoC (`[bluetooth_coc]`) is the recommended path.",
    );
    push_line(
        out,
        "# RFCOMM bonds on Linux pull in BlueZ's A2DP/HFP audio profiles by default, which",
    );
    push_line(
        out,
        "# can route an Android peer's audio to the Linux machine after pairing. See",
    );
    push_line(
        out,
        "# `.agent/memory/lessons/known-bugs.md#3` for the audio-leak history.",
    );
    push_line(
        out,
        "# Re-enable here for compatibility with Android 9 and below (no L2CAP CoC client API).",
    );
    push_line(out, "enabled = false");
    push_line(
        out,
        "# RFCOMM channel to bind and dial. Default 22 avoids common SPP conflicts.",
    );
    push_line(out, "channel = 22");
    push_line(out, "# Filter paired Bluetooth devices by name prefix.");
    push_line(out, "device_name_prefix = \"PIM-\"");
    push_line(out, "# Dial paired matching devices periodically.");
    push_line(out, "outbound_enabled = true");
    push_line(out, "# Paired-device scan cadence (ms).");
    push_line(out, "poll_interval_ms = 30000");
    push_line(
        out,
        "# Bridge established RFCOMM sessions into [transport].listen_port over loopback.",
    );
    push_line(out, "bridge_to_tcp = true");
    push_blank(out);
}

fn push_bluetooth_coc(out: &mut String) {
    push_line(out, "[bluetooth_coc]");
    push_line(
        out,
        "# Bluetooth L2CAP Connection-Oriented Channel — LE-routed counterpart to",
    );
    push_line(
        out,
        "# `[bluetooth_rfcomm]`. Same Hello/HelloAck envelope, same TCP bridge, but the",
    );
    push_line(
        out,
        "# underlying socket routes through the LE controller via `BDADDR_LE_PUBLIC` /",
    );
    push_line(
        out,
        "# `BDADDR_LE_RANDOM`. LE has no auto-registered audio profiles, so a successful",
    );
    push_line(
        out,
        "# bond never pulls in A2DP/HFP side-channels (cf. `[bluetooth_rfcomm]`).",
    );
    push_line(
        out,
        "# Default `true` — shipped as the recommended Bluetooth transport. Disable here",
    );
    push_line(
        out,
        "# to fall back exclusively to RFCOMM (e.g. for Android 9 and below).",
    );
    push_line(out, "enabled = true");
    push_line(
        out,
        "# L2CAP PSM to bind and dial. Must be inside the LE dynamic range `0x0080..=0x00FF`;",
    );
    push_line(
        out,
        "# values `0x0001..=0x007F` are SIG-assigned and reserved. Default `0x0083`.",
    );
    push_line(
        out,
        "# Android initiators read the PSM from the GAP advertisement (Phase 4) and ignore",
    );
    push_line(
        out,
        "# this value on the acceptor side (Android picks dynamically).",
    );
    push_line(out, "psm = 0x0083");
    push_line(out, "# Filter paired Bluetooth devices by name prefix.");
    push_line(out, "device_name_prefix = \"PIM-\"");
    push_line(out, "# Dial paired matching devices periodically.");
    push_line(out, "outbound_enabled = true");
    push_line(out, "# Paired-device scan cadence (ms).");
    push_line(out, "poll_interval_ms = 30000");
    push_line(
        out,
        "# Bridge established CoC sessions into [transport].listen_port over loopback.",
    );
    push_line(out, "bridge_to_tcp = true");
    push_line(
        out,
        "# When `true`, run the LE GAP advertising + scan loop alongside the paired-",
    );
    push_line(
        out,
        "# device dialer so peers find each other without an out-of-band PSM exchange.",
    );
    push_line(out, "discovery_enabled = false");
    push_line(
        out,
        "# LE-scan cadence (ms) when `discovery_enabled = true`.",
    );
    push_line(out, "inquiry_interval_ms = 60000");
    push_line(
        out,
        "# Peer address-type fed to `sockaddr_l2.l2_bdaddr_type`: 1 = LE public, 2 = LE",
    );
    push_line(
        out,
        "# random. Most Linux-paired peers are public; most smartphones are random.",
    );
    push_line(out, "peer_bdaddr_type = 1");
    push_blank(out);
}

fn push_wifi_direct(out: &mut String) {
    push_line(out, "[wifi_direct]");
    push_line(
        out,
        "# Wi-Fi Direct (IEEE 802.11 P2P) peer discovery and group formation.",
    );
    push_line(
        out,
        "# Linux backend: wpa_supplicant compiled with CONFIG_P2P=y, controlled via wpa_cli.",
    );
    push_line(
        out,
        "# macOS backend: Bonjour DNS-SD on the host's peer-to-peer Wi-Fi interface.",
    );
    push_line(
        out,
        "# Off by default — verify `wpa_cli p2p_find` returns OK before enabling on Linux.",
    );
    push_line(out, "enabled = false");
    push_line(
        out,
        &format!("interface = {:?}", default_wifi_direct_interface()),
    );
    push_line(
        out,
        "# Group Owner intent (0–15). Higher = more likely to become Group Owner. 7 = neutral.",
    );
    push_line(out, "go_intent = 7");
    push_line(out, "# P2P listen and operating channels.");
    push_line(out, "listen_channel = 6");
    push_line(out, "op_channel = 6");
    push_line(
        out,
        "# Connection method — \"pbc\" (push-button) or \"pin:<8-digit-pin>\".",
    );
    push_line(out, "connect_method = \"pbc\"");
    push_blank(out);
}

fn push_static_peers(
    out: &mut String,
    peer_example: &str,
    is_client: bool,
    is_relay: bool,
    is_gateway: bool,
) {
    push_line(
        out,
        "# Static peers are the easiest way to bootstrap a mesh in development and Docker labs.",
    );
    push_line(
        out,
        "# Each peer declares its connection mechanism. Today `tcp` and `bluetooth` are supported.",
    );
    push_line(
        out,
        "# Uncomment one or more entries and replace the example endpoint with a real peer.",
    );
    if is_gateway && !is_relay && !is_client {
        push_line(
            out,
            "# A standalone gateway can usually start without static peers and wait for inbound connections.",
        );
    }
    push_line(out, "# [[peers]]");
    push_line(out, "# mechanism = \"tcp\"");
    push_line(out, &format!("# address = {:?}", peer_example));
    push_line(out, "# label = \"replace-with-hostname-or-purpose\"");
    push_line(out, "#");
    push_line(
        out,
        "# [[peers]]  # Bluetooth PAN peer — `ip` is the address learned from the PAN neighbor table.",
    );
    push_line(out, "# mechanism = \"bluetooth\"");
    push_line(out, "# ip = \"192.168.44.2\"");
    push_line(out, "# label = \"bt-relay-a\"");

    if is_relay {
        push_line(out, "#");
        push_line(
            out,
            "# [[peers]]  # Relays commonly connect to a second upstream or downstream neighbor.",
        );
        push_line(out, "# mechanism = \"tcp\"");
        push_line(out, "# address = \"another-peer:9100\"");
        push_line(out, "# label = \"secondary-link\"");
    }

    if is_client {
        push_line(
            out,
            "# Clients usually point at a nearby relay or directly at a gateway.",
        );
    }

    if is_relay {
        push_line(
            out,
            "# Relays forward traffic for other nodes and typically keep at least one static upstream.",
        );
    }

    if is_gateway {
        push_line(
            out,
            "# Gateways may also keep static peers if they should proactively connect into the mesh.",
        );
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

pub(crate) fn unique_roles(roles: &[NodeRole]) -> BTreeSet<NodeRole> {
    roles.iter().copied().collect()
}

impl NodeRole {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Relay => "relay",
            Self::Gateway => "gateway",
        }
    }
}

pub(crate) fn default_node_name(roles: &BTreeSet<NodeRole>) -> String {
    let joined = roles
        .iter()
        .map(|role| role.as_str())
        .collect::<Vec<_>>()
        .join("-");
    format!("{joined}-node")
}

pub(crate) fn default_interface_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "utun0"
    }

    #[cfg(not(target_os = "macos"))]
    {
        "pim0"
    }
}

pub(crate) fn default_peer_example(roles: &BTreeSet<NodeRole>) -> &'static str {
    if roles.contains(&NodeRole::Gateway) && !roles.contains(&NodeRole::Relay) {
        "relay:9100"
    } else if roles.contains(&NodeRole::Relay) {
        "gateway:9100"
    } else {
        "relay-or-gateway:9100"
    }
}

pub(crate) fn default_gateway_nat_interface() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "en0"
    }

    #[cfg(not(target_os = "macos"))]
    {
        "eth0"
    }
}

pub(crate) fn default_bluetooth_interface() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "bridge0"
    }

    #[cfg(not(target_os = "macos"))]
    {
        "auto"
    }
}

pub(crate) fn default_wifi_direct_interface() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "en0"
    }

    #[cfg(not(target_os = "macos"))]
    {
        "wlan0"
    }
}

pub(crate) fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

pub(crate) fn push_blank(out: &mut String) {
    out.push('\n');
}
