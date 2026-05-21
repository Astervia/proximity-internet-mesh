use super::*;

impl BluetoothDiscovery {
    pub(super) async fn prepare_controller(&self) -> Result<(), BluetoothError> {
        self.prepare_controller_impl().await
    }

    pub(super) async fn discover_devices(&self) -> Result<Vec<DiscoveredDevice>, BluetoothError> {
        self.discover_devices_impl().await
    }

    pub(super) async fn pair_and_request_pan(
        &self,
        device: &DiscoveredDevice,
    ) -> Result<(), BluetoothError> {
        self.pair_and_request_pan_impl(device).await
    }

    pub(super) async fn resolve_pan_interfaces(
        &self,
    ) -> Result<Vec<ResolvedPanInterface>, BluetoothError> {
        self.resolve_pan_interfaces_impl().await
    }

    pub(super) async fn discover_neighbor_targets(
        &self,
        interface: &str,
    ) -> Result<Vec<SocketAddr>, BluetoothError> {
        self.discover_neighbor_targets_impl(interface).await
    }

    pub(super) async fn run_bluetoothctl<const N: usize>(
        &self,
        args: [&str; N],
    ) -> Result<(), BluetoothError> {
        self.run_bluetoothctl_capture(args).await.map(|_| ())
    }

    /// Fire `bluetoothctl <args>` without blocking the caller. The child
    /// runs to completion (or the configured `bluetoothctl_timeout_s`)
    /// in a detached tokio task; failures are logged at debug level and
    /// do not propagate. Used by the discovery loop to re-arm
    /// `scan on` / `discoverable on` periodically: BlueZ stops scan when
    /// the issuing client disconnects, and the controller's
    /// `DiscoverableTimeout` (~3 min) resets the discoverable flag back
    /// to off — both states have to be refreshed continuously for
    /// linux↔linux discovery to keep working past the first boot window.
    #[cfg(target_os = "linux")]
    pub(super) fn run_bluetoothctl_in_background(&self, args: &[&'static str]) {
        let bluetoothctl = self.bluetoothctl_command.clone();
        let timeout = self.config.bluetoothctl_timeout_s;
        let owned: Vec<&'static str> = args.to_vec();
        tokio::spawn(async move {
            let mut cmd = Command::new(&bluetoothctl);
            cmd.arg("--timeout")
                .arg(timeout.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            for a in owned {
                cmd.arg(a);
            }
            match cmd.spawn() {
                Ok(mut child) => {
                    if let Err(err) = child.wait().await {
                        debug!(%err, "background bluetoothctl child wait failed");
                    }
                }
                Err(err) => {
                    debug!(%err, "background bluetoothctl spawn failed");
                }
            }
        });
    }

    /// Resolve the local controller's BD address (`Controller XX:..:XX`).
    /// Cached after the first successful query — `bluetoothctl show` is
    /// a non-trivial round-trip and the controller MAC is stable across
    /// the daemon's lifetime.
    pub(super) async fn local_controller_mac(&self) -> Option<String> {
        self.local_controller_mac
            .get_or_init(|| async {
                #[cfg(target_os = "linux")]
                let args = ["show"];
                #[cfg(target_os = "macos")]
                let args = ["--list"];
                match self.run_bluetoothctl_capture(args).await {
                    Ok(out) => parse_controller_mac(&out),
                    Err(err) => {
                        debug!(%err, "failed to query local Bluetooth controller MAC");
                        None
                    }
                }
            })
            .await
            .clone()
    }

    pub(super) async fn run_bluetoothctl_capture<const N: usize>(
        &self,
        args: [&str; N],
    ) -> Result<String, BluetoothError> {
        let mut cmd = Command::new(&self.bluetoothctl_command);
        #[cfg(target_os = "linux")]
        {
            let timeout = self.config.bluetoothctl_timeout_s.to_string();
            cmd.arg("--timeout").arg(&timeout);
        }
        #[cfg(target_os = "macos")]
        {
            // The daemon typically runs with elevated privileges; blueutil refuses
            // to operate as root unless this override is present.
            cmd.env("BLUEUTIL_ALLOW_ROOT", "1");
        }
        for arg in args {
            cmd.arg(arg);
        }
        let output = cmd.output().await?;
        if !output.status.success() {
            return Err(BluetoothError::CommandFailed {
                command: "bluetoothctl",
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn start_pan_client(&self, mac: &str) -> Result<Child, BluetoothError> {
        let child = Command::new(&self.bt_network_command)
            .args(["-c", mac, "nap"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        Ok(child)
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn prepare_controller_impl(&self) -> Result<(), BluetoothError> {
        self.run_bluetoothctl(["power", "on"]).await?;
        self.run_bluetoothctl(["pairable", "on"]).await?;
        self.run_bluetoothctl([
            "discoverable-timeout",
            &self.config.discoverable_timeout_s.to_string(),
        ])
        .await?;
        self.run_bluetoothctl(["discoverable", "on"]).await?;
        self.run_bluetoothctl(["agent", "NoInputNoOutput"]).await?;
        self.run_bluetoothctl(["default-agent"]).await?;
        if !self.config.local_alias.is_empty() {
            self.run_bluetoothctl(["system-alias", &self.config.local_alias])
                .await?;
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub(super) async fn prepare_controller_impl(&self) -> Result<(), BluetoothError> {
        self.run_bluetoothctl(["--power", "1"]).await?;
        self.run_bluetoothctl(["--discoverable", "1"]).await?;
        if !self.config.local_alias.is_empty() {
            warn!(
                local_alias = %self.config.local_alias,
                "macOS Bluetooth backend does not set the host controller alias automatically; set the Mac Bluetooth name manually if discovery by prefix is required"
            );
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn discover_devices_impl(
        &self,
    ) -> Result<Vec<DiscoveredDevice>, BluetoothError> {
        let output = self.run_bluetoothctl_capture(["devices"]).await?;
        let local_mac = self.local_controller_mac().await;
        Ok(parse_devices_output(
            &output,
            &self.config.device_name_prefix,
            &self.config.local_alias,
            local_mac.as_deref(),
        ))
    }

    #[cfg(target_os = "macos")]
    pub(super) async fn discover_devices_impl(
        &self,
    ) -> Result<Vec<DiscoveredDevice>, BluetoothError> {
        let output = self
            .run_bluetoothctl_capture([
                "--inquiry",
                &self.config.bluetoothctl_timeout_s.to_string(),
            ])
            .await?;
        let local_mac = self.local_controller_mac().await;
        Ok(parse_blueutil_inquiry_output(
            &output,
            &self.config.device_name_prefix,
            &self.config.local_alias,
            local_mac.as_deref(),
        ))
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn pair_and_request_pan_impl(
        &self,
        device: &DiscoveredDevice,
    ) -> Result<(), BluetoothError> {
        self.run_bluetoothctl(["pair", &device.mac]).await?;
        self.run_bluetoothctl(["trust", &device.mac]).await?;
        self.run_bluetoothctl(["connect", &device.mac]).await?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn start_nap_server(&self) -> Result<Child, BluetoothError> {
        let bridge = self.config.nap_bridge.trim();
        if bridge.is_empty() {
            return Err(BluetoothError::CommandFailed {
                command: "bt-network",
                message: "serve_nap requires a non-empty nap_bridge".to_string(),
            });
        }
        if !crate::support::is_safe_interface_name(bridge) {
            return Err(BluetoothError::CommandFailed {
                command: "bt-network",
                message: format!("unsafe bridge name: {}", bridge),
            });
        }
        self.ensure_bridge_ready(bridge).await?;

        let child = Command::new(&self.bt_network_command)
            .args(["-s", "nap", bridge])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        info!(bridge, "Bluetooth NAP server started");
        Ok(child)
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn ensure_nap_server_running(
        &self,
        child: &mut Option<Child>,
    ) -> Result<(), BluetoothError> {
        if !self.config.serve_nap {
            return Ok(());
        }

        if let Some(existing) = child.as_mut() {
            if let Some(status) = existing.try_wait()? {
                warn!(?status, bridge = %self.config.nap_bridge, "Bluetooth NAP server exited; restarting");
                *child = Some(self.start_nap_server().await?);
            }
        } else {
            *child = Some(self.start_nap_server().await?);
        }

        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn ensure_bridge_ready(&self, bridge: &str) -> Result<(), BluetoothError> {
        if !crate::support::is_safe_interface_name(bridge) {
            return Err(BluetoothError::CommandFailed {
                command: "ip",
                message: format!("unsafe bridge name: {}", bridge),
            });
        }
        let bridge_path = self.sysfs_root.join(bridge);
        if !bridge_path.exists() {
            let output = Command::new(&self.ip_command)
                .args(["link", "add", "name", bridge, "type", "bridge"])
                .output()
                .await?;
            if !output.status.success() {
                let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
                if !msg.contains("File exists") && !msg.contains("already exists") {
                    return Err(BluetoothError::CommandFailed {
                        command: "ip",
                        message: format!("failed to create bridge {bridge}: {msg}"),
                    });
                }
            } else {
                info!(bridge, "Bluetooth NAP bridge created");
            }
        }

        let up_output = Command::new(&self.ip_command)
            .args(["link", "set", bridge, "up"])
            .output()
            .await?;
        if !up_output.status.success() {
            let msg = String::from_utf8_lossy(&up_output.stderr)
                .trim()
                .to_string();
            return Err(BluetoothError::CommandFailed {
                command: "ip",
                message: format!("failed to bring bridge {bridge} up: {msg}"),
            });
        }

        let addr = self.config.nap_bridge_addr.trim();
        if !addr.is_empty() {
            let output = Command::new(&self.ip_command)
                .args(["addr", "add", addr, "dev", bridge])
                .output()
                .await?;
            if output.status.success() {
                info!(bridge, %addr, "Bluetooth NAP bridge address assigned");
            } else {
                let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
                if !msg.contains("File exists") {
                    return Err(BluetoothError::CommandFailed {
                        command: "ip",
                        message: format!("failed to assign address {addr} to {bridge}: {msg}"),
                    });
                }
            }
        }

        Ok(())
    }

    /// Walk `/sys/class/net` and add any BNEP (DEVTYPE=bluetooth) interface
    /// that is not already enslaved to a bridge to the NAP bridge, then bring
    /// it up. Works around BlueZ's `bt-network -s nap` failing to auto-bridge
    /// incoming PAN connections on some distros.
    #[cfg(target_os = "linux")]
    pub(super) async fn attach_bnep_to_bridge(&self, bridge: &str) -> Result<(), BluetoothError> {
        if bridge.is_empty() {
            return Ok(());
        }
        let mut entries = match tokio::fs::read_dir(&self.sysfs_root).await {
            Ok(entries) => entries,
            Err(err) => {
                debug!(sysfs = %self.sysfs_root.display(), %err, "cannot enumerate sysfs net dir");
                return Ok(());
            }
        };
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == bridge {
                continue;
            }
            let uevent = match tokio::fs::read_to_string(entry.path().join("uevent")).await {
                Ok(contents) => contents,
                Err(_) => continue,
            };
            if !uevent.lines().any(|l| l.trim() == "DEVTYPE=bluetooth") {
                continue;
            }
            if entry.path().join("master").exists() {
                continue;
            }
            info!(iface = %name, bridge, "Bluetooth PAN client connecting; attaching BNEP interface to NAP bridge");
            let set_master = Command::new(&self.ip_command)
                .args(["link", "set", &name, "master", bridge])
                .output()
                .await?;
            if !set_master.status.success() {
                let msg = String::from_utf8_lossy(&set_master.stderr)
                    .trim()
                    .to_string();
                warn!(iface = %name, bridge, "failed to attach BNEP to bridge: {msg}");
                continue;
            }
            let set_up = Command::new(&self.ip_command)
                .args(["link", "set", &name, "up"])
                .output()
                .await?;
            if !set_up.status.success() {
                let msg = String::from_utf8_lossy(&set_up.stderr).trim().to_string();
                warn!(iface = %name, "failed to bring BNEP interface up: {msg}");
                continue;
            }
            info!(iface = %name, bridge, "Bluetooth BNEP interface attached to NAP bridge");
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn install_bluetooth_masquerade(&self) -> Result<(), BluetoothError> {
        let Some(nat_iface) = self.nat_interface.as_deref() else {
            debug!("Bluetooth MASQUERADE not installed; no nat_interface configured");
            return Ok(());
        };
        if !crate::support::is_safe_interface_name(nat_iface) {
            return Err(BluetoothError::CommandFailed {
                command: "iptables",
                message: format!("unsafe nat_interface name: {}", nat_iface),
            });
        }
        let (gateway, prefix) = match parse_ipv4_cidr(&self.config.nap_bridge_addr) {
            Ok(v) => v,
            Err(msg) => {
                warn!(
                    addr = %self.config.nap_bridge_addr,
                    error = %msg,
                    "Bluetooth MASQUERADE: invalid nap_bridge_addr; skipping"
                );
                return Ok(());
            }
        };
        let (network, _) = subnet_network(gateway, prefix);
        let subnet = format!("{network}/{prefix}");

        let _ = Command::new("sysctl")
            .args(["-w", "net.ipv4.ip_forward=1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        let post_args = [
            "-t",
            "nat",
            "-A",
            "POSTROUTING",
            "-s",
            subnet.as_str(),
            "-o",
            nat_iface,
            "-j",
            "MASQUERADE",
        ];
        self.iptables_ensure(&post_args).await?;

        let fwd_args = ["-A", "FORWARD", "-s", subnet.as_str(), "-j", "ACCEPT"];
        self.iptables_ensure(&fwd_args).await?;

        info!(%subnet, nat_iface, "Bluetooth MASQUERADE installed");
        Ok(())
    }

    /// Reverse of `install_bluetooth_masquerade`: removes the POSTROUTING
    /// MASQUERADE and FORWARD ACCEPT rules we installed. Idempotent and
    /// best-effort; errors are logged, not propagated.
    #[cfg(target_os = "linux")]
    pub(super) async fn uninstall_bluetooth_masquerade(&self) {
        let Some(nat_iface) = self.nat_interface.as_deref() else {
            return;
        };
        if !crate::support::is_safe_interface_name(nat_iface) {
            return;
        }
        let (gateway, prefix) = match parse_ipv4_cidr(&self.config.nap_bridge_addr) {
            Ok(v) => v,
            Err(_) => return,
        };
        let (network, _) = subnet_network(gateway, prefix);
        let subnet = format!("{network}/{prefix}");

        let post_args = [
            "-t",
            "nat",
            "-D",
            "POSTROUTING",
            "-s",
            subnet.as_str(),
            "-o",
            nat_iface,
            "-j",
            "MASQUERADE",
        ];
        self.iptables_delete_while_present(&post_args).await;

        let fwd_args = ["-D", "FORWARD", "-s", subnet.as_str(), "-j", "ACCEPT"];
        self.iptables_delete_while_present(&fwd_args).await;

        info!(%subnet, nat_iface, "Bluetooth MASQUERADE removed");
    }

    /// Repeatedly run `iptables -D ...` until it reports the rule is gone.
    /// `iptables_ensure` uses `-C` before `-A`, so at most one copy should
    /// exist, but loop defensively up to a small bound in case the daemon
    /// was restarted without cleanup in the past.
    #[cfg(target_os = "linux")]
    pub(super) async fn iptables_delete_while_present(&self, delete_args: &[&str]) {
        for _ in 0..8 {
            let mut check: Vec<&str> = delete_args.to_vec();
            if let Some(pos) = check.iter().position(|a| *a == "-D") {
                check[pos] = "-C";
            }
            let present = Command::new(&self.iptables_command)
                .args(&check)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if !present {
                return;
            }
            let _ = Command::new(&self.iptables_command)
                .args(delete_args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }

    /// Bring the NAP bridge down and delete it, if it exists.
    /// Best-effort: errors are logged, not propagated.
    #[cfg(target_os = "linux")]
    pub(super) async fn delete_bridge_if_present(&self, bridge: &str) {
        if bridge.is_empty() {
            return;
        }
        if !crate::support::is_safe_interface_name(bridge) {
            return;
        }
        // Best-effort: unconditionally attempt the delete. Bridge existence
        // cannot be inferred from `sysfs_root` since tests override that to
        // a fake tree while the real bridge is created via the `ip` command.
        let down = Command::new(&self.ip_command)
            .args(["link", "set", bridge, "down"])
            .output()
            .await;
        if let Ok(out) = &down {
            if !out.status.success() {
                let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if !msg.is_empty() && !msg.contains("Cannot find device") {
                    debug!(bridge, "ip link set down: {msg}");
                }
            }
        }
        let del = Command::new(&self.ip_command)
            .args(["link", "delete", bridge])
            .output()
            .await;
        match del {
            Ok(out) if out.status.success() => {
                info!(bridge, "Bluetooth NAP bridge deleted");
            }
            Ok(out) => {
                let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if !msg.contains("Cannot find device") && !msg.contains("does not exist") {
                    warn!(bridge, "ip link delete failed: {msg}");
                }
            }
            Err(err) => {
                warn!(bridge, "ip link delete errored: {err}");
            }
        }
    }

    /// Shut down Linux-specific resources in order: child processes first,
    /// then iptables rules, then the bridge. Safe to call even when some
    /// resources were never created (e.g. NAP disabled).
    #[cfg(target_os = "linux")]
    pub(super) async fn teardown_linux(
        &self,
        nap_server: &mut Option<Child>,
        dnsmasq_child: &mut Option<Child>,
        dhclient_children: &mut HashMap<String, Child>,
        pan_clients: &mut HashMap<String, Child>,
    ) {
        if let Some(mut child) = nap_server.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        if let Some(mut child) = dnsmasq_child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        for (_, mut child) in dhclient_children.drain() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        for (_, mut child) in pan_clients.drain() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        if self.config.serve_nap {
            self.uninstall_bluetooth_masquerade().await;
            let bridge = self.config.nap_bridge.trim().to_string();
            self.delete_bridge_if_present(&bridge).await;
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn iptables_ensure(&self, args: &[&str]) -> Result<(), BluetoothError> {
        let mut check_args: Vec<&str> = args.to_vec();
        if let Some(pos) = check_args.iter().position(|a| *a == "-A") {
            check_args[pos] = "-C";
        }
        let check = Command::new(&self.iptables_command)
            .args(&check_args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
        if check.success() {
            return Ok(());
        }
        let output = Command::new(&self.iptables_command)
            .args(args)
            .output()
            .await?;
        if !output.status.success() {
            return Err(BluetoothError::CommandFailed {
                command: "iptables",
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn resolve_dhcp_dns(&self) -> String {
        if let Some(custom) = self.config.dhcp_dns.as_deref() {
            let trimmed = custom.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        match tokio::fs::read_to_string(&self.resolv_conf_path).await {
            Ok(content) => {
                let servers: Vec<String> = content
                    .lines()
                    .filter_map(|line| {
                        let line = line.trim();
                        if line.starts_with('#') {
                            return None;
                        }
                        let stripped = line.strip_prefix("nameserver")?.trim();
                        if stripped.is_empty() {
                            return None;
                        }
                        Some(stripped.split_whitespace().next()?.to_string())
                    })
                    .collect();
                if servers.is_empty() {
                    "1.1.1.1,8.8.8.8".to_string()
                } else {
                    servers.join(",")
                }
            }
            Err(err) => {
                debug!(
                    path = %self.resolv_conf_path.display(),
                    %err,
                    "unable to read resolv.conf; falling back to public DNS"
                );
                "1.1.1.1,8.8.8.8".to_string()
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn start_dnsmasq(&self, bridge: &str) -> Result<Child, BluetoothError> {
        if !crate::support::is_safe_interface_name(bridge) {
            return Err(BluetoothError::CommandFailed {
                command: "dnsmasq",
                message: format!("unsafe bridge name: {}", bridge),
            });
        }
        let (gateway, prefix) = parse_ipv4_cidr(&self.config.nap_bridge_addr).map_err(|msg| {
            BluetoothError::CommandFailed {
                command: "dnsmasq",
                message: msg,
            }
        })?;
        let range = match self.config.dhcp_range.as_deref().map(str::trim) {
            Some(explicit) if !explicit.is_empty() => explicit.to_string(),
            _ => default_dhcp_range(gateway, prefix).ok_or_else(|| {
                BluetoothError::CommandFailed {
                    command: "dnsmasq",
                    message: format!(
                        "unable to derive DHCP range from {}",
                        self.config.nap_bridge_addr
                    ),
                }
            })?,
        };
        let lease = self.config.dhcp_lease_time.trim();
        let dns = self.resolve_dhcp_dns().await;
        let dhcp_range_arg = if lease.is_empty() {
            format!("--dhcp-range={range}")
        } else {
            format!("--dhcp-range={range},{lease}")
        };
        let router_arg = format!("--dhcp-option=3,{gateway}");
        let dns_arg = format!("--dhcp-option=6,{dns}");
        let iface_arg = format!("--interface={bridge}");
        let child = Command::new(&self.dnsmasq_command)
            .args([
                "--keep-in-foreground",
                "--log-facility=-",
                "--port=0",
                "--bind-interfaces",
                "--except-interface=lo",
                iface_arg.as_str(),
                dhcp_range_arg.as_str(),
                router_arg.as_str(),
                dns_arg.as_str(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        info!(bridge, %range, %gateway, %dns, "Bluetooth DHCP server started");
        Ok(child)
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn ensure_dnsmasq_running(
        &self,
        bridge: &str,
        child: &mut Option<Child>,
    ) -> Result<(), BluetoothError> {
        if let Some(existing) = child.as_mut() {
            if let Some(status) = existing.try_wait()? {
                warn!(?status, bridge, "Bluetooth DHCP server exited; restarting");
                *child = Some(self.start_dnsmasq(bridge).await?);
            }
        } else {
            *child = Some(self.start_dnsmasq(bridge).await?);
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn start_dhclient(&self, interface: &str) -> Result<Child, BluetoothError> {
        if !crate::support::is_safe_interface_name(interface) {
            return Err(BluetoothError::CommandFailed {
                command: "dhclient",
                message: format!("unsafe interface name: {}", interface),
            });
        }
        let child = Command::new(&self.dhclient_command)
            .args(["-d", "-v", interface])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        info!(interface, "Bluetooth DHCP client started");
        Ok(child)
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn ensure_dhclient_running(
        &self,
        interface: &str,
        children: &mut HashMap<String, Child>,
    ) -> Result<(), BluetoothError> {
        let mut should_start = false;
        if let Some(existing) = children.get_mut(interface) {
            if let Some(status) = existing.try_wait()? {
                warn!(
                    ?status,
                    interface, "Bluetooth DHCP client exited; restarting"
                );
                should_start = true;
            }
        } else {
            should_start = true;
        }

        if should_start {
            if let Some(mut existing) = children.remove(interface) {
                let _ = existing.kill().await;
                let _ = existing.wait().await;
            }
            children.insert(interface.to_string(), self.start_dhclient(interface).await?);
        }

        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn prune_dhclient_children(
        &self,
        active_interfaces: &[ResolvedPanInterface],
        children: &mut HashMap<String, Child>,
    ) {
        let active: HashSet<&str> = active_interfaces
            .iter()
            .map(|interface| interface.name.as_str())
            .collect();
        let stale: Vec<String> = children
            .keys()
            .filter(|name| !active.contains(name.as_str()))
            .cloned()
            .collect();
        for interface in stale {
            if let Some(mut child) = children.remove(&interface) {
                warn!(interface = %interface, "Bluetooth DHCP client interface disappeared; stopping");
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) async fn pair_and_request_pan_impl(
        &self,
        device: &DiscoveredDevice,
    ) -> Result<(), BluetoothError> {
        self.run_bluetoothctl(["--pair", &device.mac]).await?;
        self.run_bluetoothctl(["--connect", &device.mac]).await?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn resolve_pan_interfaces_impl(
        &self,
    ) -> Result<Vec<ResolvedPanInterface>, BluetoothError> {
        let candidates = list_pan_candidates(&self.sysfs_root).await?;

        let interfaces = select_pan_interfaces(
            &candidates,
            preferred_interface_hint(&self.config.interface),
            self.config
                .serve_nap
                .then_some(self.config.nap_bridge.as_str()),
        );
        if !interfaces.is_empty() {
            return Ok(interfaces);
        }

        if !candidates.is_empty() {
            debug!(
                configured_interface = %self.config.interface,
                candidates = %format_candidate_summary(&candidates),
                "Bluetooth PAN interface not ready yet"
            );
        }
        Ok(Vec::new())
    }

    #[cfg(target_os = "macos")]
    pub(super) async fn resolve_pan_interfaces_impl(
        &self,
    ) -> Result<Vec<ResolvedPanInterface>, BluetoothError> {
        let interface = resolve_macos_pan_interface_hint(&self.config.interface);
        if !crate::support::is_safe_interface_name(interface) {
            return Err(BluetoothError::CommandFailed {
                command: "ifconfig",
                message: format!("unsafe interface name: {}", interface),
            });
        }
        let output = Command::new("ifconfig").arg(interface).output().await?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        Ok(
            is_ready_ifconfig_output(&String::from_utf8_lossy(&output.stdout))
                .then(|| ResolvedPanInterface {
                    name: interface.to_string(),
                    source: if self.config.interface.trim() == "auto" {
                        "auto-default"
                    } else {
                        "configured"
                    },
                })
                .into_iter()
                .collect(),
        )
    }

    #[cfg(target_os = "linux")]
    pub(super) async fn discover_neighbor_targets_impl(
        &self,
        interface: &str,
    ) -> Result<Vec<SocketAddr>, BluetoothError> {
        if !crate::support::is_safe_interface_name(interface) {
            return Err(BluetoothError::CommandFailed {
                command: "ip",
                message: format!("unsafe interface name: {}", interface),
            });
        }
        let scope_id = interface_index(interface);
        let output = Command::new(&self.ip_command)
            .args(["neigh", "show", "dev", interface])
            .output()
            .await?;

        if !output.status.success() {
            return Err(BluetoothError::CommandFailed {
                command: "ip",
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        Ok(parse_neighbor_output(
            &String::from_utf8_lossy(&output.stdout),
            self.listen_port,
            scope_id,
        ))
    }

    #[cfg(target_os = "macos")]
    pub(super) async fn discover_neighbor_targets_impl(
        &self,
        interface: &str,
    ) -> Result<Vec<SocketAddr>, BluetoothError> {
        if !crate::support::is_safe_interface_name(interface) {
            return Err(BluetoothError::CommandFailed {
                command: "arp",
                message: format!("unsafe interface name: {}", interface),
            });
        }
        let output = Command::new(&self.ip_command)
            .args(["-an", "-i", interface])
            .output()
            .await?;

        if !output.status.success() {
            return Err(BluetoothError::CommandFailed {
                command: "arp",
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        Ok(parse_arp_output(
            &String::from_utf8_lossy(&output.stdout),
            interface,
            self.listen_port,
        ))
    }
}
