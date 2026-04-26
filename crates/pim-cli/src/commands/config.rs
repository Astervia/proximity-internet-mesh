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

// ── `pim route` ──────────────────────────────────────────────────────────────

pub(crate) fn render_config_template(roles: &[NodeRole], override_name: Option<&str>) -> String {
    let roles = unique_roles(roles);
    let is_gateway = roles.contains(&NodeRole::Gateway);
    let is_relay = roles.contains(&NodeRole::Relay);
    let is_client = roles.contains(&NodeRole::Client);
    let node_name = override_name
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_node_name(&roles));
    let mesh_ip = default_mesh_ip(&roles);
    let peer_example = default_peer_example(&roles);
    let roles_label = roles
        .iter()
        .map(|role| role.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    let mut out = String::new();
    push_line(&mut out, "# Proximity Internet Mesh configuration template");
    push_line(&mut out, &format!("# Roles enabled: {roles_label}"));
    push_line(
        &mut out,
        "# Edit the values below, save the file, then start the daemon with:",
    );
    push_line(&mut out, "#   sudo pim up --config /etc/pim/pim.toml");
    push_blank(&mut out);

    push_line(&mut out, "[node]");
    push_line(
        &mut out,
        "# Human-readable node name used in logs and status output.",
    );
    push_line(&mut out, &format!("name = {:?}", node_name));
    push_line(
        &mut out,
        "# Writable state directory for generated keys and runtime data.",
    );
    push_line(&mut out, "data_dir = \"/var/lib/pim\"");
    push_blank(&mut out);

    push_line(&mut out, "[interface]");
    push_line(
        &mut out,
        "# TUN interface exposed by the daemon. Linux commonly uses pim0; macOS must use a utunN name.",
    );
    push_line(&mut out, &format!("name = {:?}", default_interface_name()));
    push_line(
        &mut out,
        "# Use a static CIDR for predictable labs or \"auto\" to request an address from a gateway.",
    );
    push_line(&mut out, &format!("mesh_ip = {:?}", mesh_ip));
    push_line(
        &mut out,
        "# Optional static IPv6 ULA on the mesh TUN. Leave commented to run IPv4-only.",
    );
    push_line(&mut out, "# mesh_ipv6 = \"fd77::10/64\"");
    push_line(
        &mut out,
        "# Keep this aligned with the mesh MTU expected by other peers.",
    );
    push_line(&mut out, "mtu = 1400");
    push_blank(&mut out);

    push_line(&mut out, "[discovery]");
    push_line(
        &mut out,
        "# Broadcast-based peer discovery. Static peers below are still the simplest way to start.",
    );
    push_line(&mut out, "broadcast_interval_ms = 5000");
    push_line(&mut out, "peer_timeout_ms = 30000");
    push_line(
        &mut out,
        "# Optional 64-hex-character group key. When set, only nodes with the same key can decode discovery broadcasts.",
    );
    push_line(
        &mut out,
        "# shared_key = \"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\"",
    );
    push_blank(&mut out);

    push_line(&mut out, "[transport]");
    push_line(
        &mut out,
        "# Transport backend implemented in this repository today.",
    );
    push_line(&mut out, "type = \"tcp\"");
    push_line(
        &mut out,
        "# TCP port this node listens on for direct peer sessions.",
    );
    push_line(&mut out, "listen_port = 9100");
    push_blank(&mut out);

    push_line(&mut out, "[routing]");
    push_line(
        &mut out,
        "# Distance-vector settings used for route propagation and expiry.",
    );
    push_line(&mut out, "max_hops = 10");
    push_line(&mut out, "algorithm = \"distance-vector\"");
    push_line(&mut out, "route_expiry_s = 300");
    push_blank(&mut out);

    if is_gateway {
        push_line(&mut out, "[gateway]");
        push_line(
            &mut out,
            "# Enable NAT and internet egress on a node with upstream connectivity.",
        );
        push_line(&mut out, "enabled = true");
        push_line(
            &mut out,
            "# Replace this with the real internet-facing interface on the host.",
        );
        push_line(
            &mut out,
            &format!("nat_interface = {:?}", default_gateway_nat_interface()),
        );
        push_line(
            &mut out,
            "# Maximum concurrent gateway connection-tracking entries.",
        );
        push_line(&mut out, "max_connections = 200");
    } else {
        push_line(
            &mut out,
            "# [gateway]  # Uncomment this section only on a node that should provide internet access.",
        );
        push_line(&mut out, "# enabled = true");
        push_line(
            &mut out,
            &format!("# nat_interface = {:?}", default_gateway_nat_interface()),
        );
        push_line(&mut out, "# max_connections = 200");
    }
    push_blank(&mut out);

    push_line(&mut out, "[security]");
    push_line(
        &mut out,
        "# The daemon creates this Ed25519 private key on first startup if it does not exist.",
    );
    push_line(&mut out, "key_file = \"/var/lib/pim/node.key\"");
    push_line(
        &mut out,
        "# Reject direct peer sessions that do not complete the authenticated handshake.",
    );
    push_line(&mut out, "require_encryption = true");
    push_line(
        &mut out,
        "# Authorization policy: allow_all, allow_list, or trust_on_first_use.",
    );
    push_line(&mut out, "authorization_policy = \"allow_all\"");
    push_line(
        &mut out,
        "# Used only when authorization_policy = \"allow_list\".",
    );
    push_line(
        &mut out,
        "# authorized_peers = [\"0123456789abcdef0123456789abcdef\"]",
    );
    push_line(
        &mut out,
        "# Used only when authorization_policy = \"trust_on_first_use\".",
    );
    push_line(
        &mut out,
        "trust_store_file = \"/var/lib/pim/trusted-peers.toml\"",
    );
    push_blank(&mut out);

    push_line(
        &mut out,
        "# Static peers are the easiest way to bootstrap a mesh in development and Docker labs.",
    );
    push_line(
        &mut out,
        "# Each peer declares its connection mechanism. Today `tcp` and `bluetooth` are supported.",
    );
    push_line(
        &mut out,
        "# Uncomment one or more entries and replace the example endpoint with a real peer.",
    );
    if is_gateway && !is_relay && !is_client {
        push_line(
            &mut out,
            "# A standalone gateway can usually start without static peers and wait for inbound connections.",
        );
    }
    push_line(&mut out, "# [[peers]]");
    push_line(&mut out, "# mechanism = \"tcp\"");
    push_line(&mut out, &format!("# address = {:?}", peer_example));
    push_line(&mut out, "# label = \"replace-with-hostname-or-purpose\"");

    if is_relay {
        push_line(
            &mut out,
            "# [[peers]]  # Relays commonly connect to a second upstream or downstream neighbor.",
        );
        push_line(&mut out, "# mechanism = \"tcp\"");
        push_line(&mut out, "# address = \"another-peer:9100\"");
        push_line(&mut out, "# label = \"secondary-link\"");
    }

    if is_client {
        push_line(
            &mut out,
            "# Clients usually point at a nearby relay or directly at a gateway.",
        );
    }

    if is_relay {
        push_line(
            &mut out,
            "# Relays forward traffic for other nodes and typically keep at least one static upstream.",
        );
    }

    if is_gateway {
        push_line(
            &mut out,
            "# Gateways may also keep static peers if they should proactively connect into the mesh.",
        );
    }

    out
}

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

pub(crate) fn default_mesh_ip(roles: &BTreeSet<NodeRole>) -> &'static str {
    if roles.contains(&NodeRole::Gateway) {
        "10.77.0.1/24"
    } else if roles.contains(&NodeRole::Relay) {
        "10.77.0.10/24"
    } else {
        "auto"
    }
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

pub(crate) fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

pub(crate) fn push_blank(out: &mut String) {
    out.push('\n');
}
