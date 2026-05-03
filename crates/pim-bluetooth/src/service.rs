use super::*;

impl BluetoothDiscovery {
    /// Run the Bluetooth service until cancellation.
    pub async fn run(self, cancel: CancellationToken) -> Result<(), BluetoothError> {
        if self.static_targets.is_empty()
            && !self.config.auto_discover_peers
            && !self.config.radio_discovery_enabled
            && !self.config.serve_nap
        {
            warn!(
                interface = %self.config.interface,
                "Bluetooth enabled with neither static peers, PAN neighbor discovery, nor radio discovery; skipping"
            );
            return Ok(());
        }

        info!(
            interface = %self.config.interface,
            connect_pan = self.config.connect_pan,
            serve_nap = self.config.serve_nap,
            nap_bridge = %self.config.nap_bridge,
            static_peers = self.static_targets.len(),
            radio_discovery = self.config.radio_discovery_enabled,
            auto_discover_pan_peers = self.config.auto_discover_peers,
            "Bluetooth service starting"
        );

        if self.config.radio_discovery_enabled {
            self.prepare_controller().await?;
            self.run_bluetoothctl(["scan", "on"]).await?;
            // Warm the local controller MAC cache so the very first
            // scan_interval tick already filters by MAC instead of
            // alias. Best-effort — failures fall back to alias filter.
            self.local_controller_mac().await;
        }

        // Re-arm `discoverable on` well before the controller's
        // DiscoverableTimeout (default 180 s) expires. Without this, a
        // peer that comes up minutes after our daemon never finds us
        // because the controller has stopped responding to inquiry-scan.
        // Runs as a detached task — the periodic re-arm doesn't need to
        // coordinate with the main discovery select! loop.
        #[cfg(target_os = "linux")]
        if self.config.radio_discovery_enabled {
            let bluetoothctl = self.bluetoothctl_command.clone();
            let timeout = self.config.bluetoothctl_timeout_s;
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_secs(60));
                ticker.tick().await; // skip the immediate first tick (prepare_controller already armed it)
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = ticker.tick() => {
                            let mut cmd = Command::new(&bluetoothctl);
                            cmd.arg("--timeout")
                                .arg(timeout.to_string())
                                .arg("discoverable")
                                .arg("on")
                                .stdout(Stdio::null())
                                .stderr(Stdio::null())
                                .kill_on_drop(true);
                            match cmd.spawn() {
                                Ok(mut child) => { let _ = child.wait().await; }
                                Err(err) => debug!(%err, "discoverable keepalive spawn failed"),
                            }
                        }
                    }
                }
            });
        }

        #[cfg(target_os = "linux")]
        let mut nap_server = if self.config.serve_nap {
            self.install_bluetooth_masquerade().await?;
            Some(self.start_nap_server().await?)
        } else {
            None
        };
        #[cfg(target_os = "linux")]
        let mut dnsmasq_child: Option<Child> = None;
        #[cfg(target_os = "linux")]
        let mut dhclient_children: HashMap<String, Child> = HashMap::new();
        #[cfg(target_os = "linux")]
        let mut pan_clients: HashMap<String, Child> = HashMap::new();

        let mut active_interfaces: Vec<ResolvedPanInterface> = Vec::new();
        let startup_deadline =
            Instant::now() + Duration::from_millis(self.config.startup_timeout_ms);
        let mut emitted_static = false;
        let mut seen_addrs: HashSet<SocketAddr> = HashSet::new();
        let mut seen_macs: HashSet<String> = HashSet::new();

        let mut interface_interval =
            tokio::time::interval(Duration::from_millis(self.config.poll_interval_ms.max(1)));
        let mut scan_interval =
            tokio::time::interval(Duration::from_millis(self.config.scan_interval_ms.max(1)));
        let mut peer_interval = tokio::time::interval(Duration::from_millis(
            self.config.peer_discovery_interval_ms.max(1),
        ));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    #[cfg(target_os = "linux")]
                    {
                        self.teardown_linux(
                            &mut nap_server,
                            &mut dnsmasq_child,
                            &mut dhclient_children,
                            &mut pan_clients,
                        )
                        .await;
                    }
                    debug!("Bluetooth service cancelled");
                    return Ok(());
                }
                _ = interface_interval.tick() => {
                    #[cfg(target_os = "linux")]
                    {
                        self.ensure_nap_server_running(&mut nap_server).await?;
                        if self.config.serve_nap {
                            let bridge = self.config.nap_bridge.trim().to_string();
                            if !bridge.is_empty() {
                                self.attach_bnep_to_bridge(&bridge).await?;
                                if self.config.dhcp_enabled {
                                    self.ensure_dnsmasq_running(&bridge, &mut dnsmasq_child).await?;
                                }
                            }
                        }
                    }

                    let resolved = self.resolve_pan_interfaces().await?;
                    if active_interfaces != resolved {
                        let previous: HashSet<&str> =
                            active_interfaces.iter().map(|iface| iface.name.as_str()).collect();
                        let current: HashSet<&str> =
                            resolved.iter().map(|iface| iface.name.as_str()).collect();

                        for interface in &resolved {
                            if !previous.contains(interface.name.as_str()) {
                                info!(
                                    configured_interface = %self.config.interface,
                                    resolved_interface = %interface.name,
                                    source = interface.source,
                                    "Bluetooth PAN interface selected"
                                );
                            }
                        }

                        for previous_interface in &active_interfaces {
                            if !current.contains(previous_interface.name.as_str()) {
                                warn!(
                                    previous_interface = %previous_interface.name,
                                    "Bluetooth PAN interface disappeared; waiting for it to return"
                                );
                            }
                        }

                        active_interfaces = resolved;
                    }

                    #[cfg(target_os = "linux")]
                    if self.config.request_dhcp
                        && self.config.connect_pan
                        && !self.config.serve_nap
                    {
                        self.prune_dhclient_children(&active_interfaces, &mut dhclient_children)
                            .await;
                        for interface in &active_interfaces {
                            if let Err(err) = self
                                .ensure_dhclient_running(&interface.name, &mut dhclient_children)
                                .await
                            {
                                warn!(
                                    interface = %interface.name,
                                    dhclient = %self.dhclient_command.display(),
                                    %err,
                                    "Bluetooth DHCP client unavailable; continuing without DHCP (peer discovery still works via IPv6 link-local)"
                                );
                            }
                        }
                    }

                    if active_interfaces.is_empty() && Instant::now() >= startup_deadline {
                        warn!(
                            interface = %self.config.interface,
                            timeout_ms = self.config.startup_timeout_ms,
                            "Bluetooth PAN interface did not become ready before timeout"
                        );
                        if !(self.config.radio_discovery_enabled && self.config.connect_pan) {
                            warn!(
                                interface = %self.config.interface,
                                "Bluetooth PAN discovery exiting: interface never ready and radio scan is disabled"
                            );
                            return Ok(());
                        }
                    }

                    if !active_interfaces.is_empty() && !emitted_static {
                        for addr in &self.static_targets {
                            info!(%addr, "Bluetooth PAN static peer ready");
                            if self.peer_tx.send(*addr).await.is_err() {
                                return Ok(());
                            }
                            seen_addrs.insert(*addr);
                        }
                        emitted_static = true;
                    }
                }
                _ = scan_interval.tick(), if self.config.radio_discovery_enabled && self.config.connect_pan => {
                    // Re-arm BlueZ inquiry: `bluetoothctl --timeout N
                    // scan on` from prepare_controller stops issuing
                    // StartDiscovery once the bluetoothctl client exits
                    // (after `bluetoothctl_timeout_s`). Without this
                    // periodic re-fire, the daemon's "scan started"
                    // log lines below are misleading — they only read
                    // the cached device list, no fresh inquiry happens.
                    #[cfg(target_os = "linux")]
                    self.run_bluetoothctl_in_background(&["scan", "on"]);

                    #[cfg(target_os = "linux")]
                    {
                        pan_clients.retain(|mac, child| match child.try_wait() {
                            Ok(Some(status)) => {
                                warn!(%mac, ?status, "Bluetooth PAN client (bt-network) exited; will retry on next scan");
                                seen_macs.remove(mac);
                                false
                            }
                            Ok(None) => true,
                            Err(err) => {
                                warn!(%mac, %err, "failed to poll bt-network child; dropping handle");
                                seen_macs.remove(mac);
                                false
                            }
                        });
                    }

                    info!(
                        prefix = %self.config.device_name_prefix,
                        active_connections = seen_macs.len(),
                        "Bluetooth radio scan started"
                    );
                    let devices = self.discover_devices().await?;
                    info!(
                        found = devices.len(),
                        new = devices.iter().filter(|d| !seen_macs.contains(&d.mac)).count(),
                        "Bluetooth radio scan complete"
                    );
                    for device in devices {
                        if seen_macs.contains(&device.mac) {
                            continue;
                        }
                        match self.pair_and_request_pan(&device).await {
                            Ok(()) => {
                                #[cfg(target_os = "linux")]
                                {
                                    match self.start_pan_client(&device.mac).await {
                                        Ok(child) => {
                                            info!(mac = %device.mac, name = %device.name, "Bluetooth radio-discovered peer prepared");
                                            pan_clients.insert(device.mac.clone(), child);
                                            seen_macs.insert(device.mac);
                                        }
                                        Err(err) => {
                                            warn!(mac = %device.mac, name = %device.name, "Bluetooth bt-network spawn failed: {err}");
                                        }
                                    }
                                }
                                #[cfg(not(target_os = "linux"))]
                                {
                                    info!(mac = %device.mac, name = %device.name, "Bluetooth radio-discovered peer prepared");
                                    seen_macs.insert(device.mac);
                                }
                            }
                            Err(err) => {
                                warn!(mac = %device.mac, name = %device.name, "Bluetooth radio discovery failed: {err}");
                            }
                        }
                    }
                }
                _ = peer_interval.tick(), if !active_interfaces.is_empty() && self.config.auto_discover_peers => {
                    let mut discovered_all: HashSet<SocketAddr> = HashSet::new();
                    let mut missing_interfaces: HashSet<String> = HashSet::new();

                    for interface in &active_interfaces {
                        let discovered = match self.discover_neighbor_targets(&interface.name).await {
                            Ok(discovered) => discovered,
                            Err(err) if err.is_missing_device_error() => {
                                warn!(
                                    interface = %interface.name,
                                    "Bluetooth PAN interface disappeared during neighbor scan; waiting for it to return"
                                );
                                missing_interfaces.insert(interface.name.clone());
                                continue;
                            }
                            Err(err) => return Err(err),
                        };

                        for addr in discovered {
                            discovered_all.insert(addr);
                            if seen_addrs.insert(addr) {
                                info!(%addr, interface = %interface.name, "Bluetooth PAN discovered peer addr");
                                if self.peer_tx.send(addr).await.is_err() {
                                    return Ok(());
                                }
                            }
                        }
                    }

                    seen_addrs.retain(|addr| {
                        self.static_targets.contains(addr) || discovered_all.contains(addr)
                    });

                    if !missing_interfaces.is_empty() {
                        active_interfaces
                            .retain(|interface| !missing_interfaces.contains(&interface.name));
                    }
                }
            }
        }
    }
}
