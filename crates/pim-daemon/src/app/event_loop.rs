use super::*;

pub(super) async fn run_event_loop(state: Arc<DaemonState>) -> Result<()> {
    let mut tun_buf = vec![0u8; 65536];
    // Per-gateway X25519 public keys — learned from each gateway's direct heartbeat.
    // For a gateway node, pre-populate with own key so loopback internet flows encrypt correctly.
    let mut gw_x25519_by_id: HashMap<NodeId, [u8; 32]> = HashMap::new();
    if state.is_gateway {
        gw_x25519_by_id.insert(state.self_id, state.own_x25519_pub);
    }

    info!(self_id = %state.self_id, is_gateway = state.is_gateway, "event loop started");

    loop {
        tokio::select! {
            // ── TUN → mesh ──────────────────────────────────────────────────
            res = state.tun.read_packet(&mut tun_buf) => {
                let n = match res {
                    Ok(n) => n,
                    Err(e) => { error!("TUN read: {e}"); break; }
                };
                let packet = &tun_buf[..n];
                debug!(bytes = n, "TUN read");

                let Some(ip_version) = packet_ip_version(packet) else {
                    debug!(bytes = n, "empty packet from TUN; dropping");
                    continue;
                };
                debug!(bytes = n, ip_version, "TUN pkt: before routing lookup");
                let (dst_id, next_hop_id, mut flags) = if ip_version == 4 {
                    if n < 20 {
                        debug!(bytes = n, "short IPv4 packet from TUN; dropping");
                        continue;
                    }

                    let dest_ip = Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]);

                    if dest_ip == Ipv4Addr::from(state.mesh_ip.load(Ordering::Relaxed)) {
                        continue;
                    }

                    debug!(%dest_ip, "TUN pkt: acquiring routing lock (v4)");
                    let mesh_lookup = state.routing.lock().await.lookup_mesh_ip(dest_ip);
                    debug!(%dest_ip, found = mesh_lookup.is_some(), "TUN pkt: routing lookup done (v4)");
                    match mesh_lookup {
                        Some((dst_id, next_hop)) => (dst_id, next_hop, DataFlags::empty()),
                        None => {
                            debug!(%dest_ip, "TUN pkt: acquiring routing lock for gw_route (v4)");
                            let gw_route = state.routing.lock().await.nearest_gateway_route();
                            debug!(%dest_ip, has_gw = gw_route.is_some(), "TUN pkt: gw_route done (v4)");
                            match gw_route {
                                Some((gw_id, next_hop)) => (gw_id, next_hop, DataFlags::IS_INTERNET),
                                None => {
                                    let peers = state.transport.connected_peers();
                                    if peers.is_empty() {
                                        debug!("no route and no peers; dropping packet");
                                        continue;
                                    }
                                    (peers[0], peers[0], DataFlags::IS_INTERNET)
                                }
                            }
                        }
                    }
                } else if ip_version == 6 {
                    debug!("TUN pkt: acquiring routing lock (v6)");
                    let gw_route = state.routing.lock().await.nearest_gateway_route();
                    debug!(has_gw = gw_route.is_some(), "TUN pkt: gw_route done (v6)");
                    match gw_route {
                        Some((gw_id, next_hop)) => (gw_id, next_hop, DataFlags::IS_INTERNET),
                        None => {
                            let peers = state.transport.connected_peers();
                            if peers.is_empty() {
                                debug!("no IPv6 gateway route and no peers; dropping packet");
                                continue;
                            }
                            (peers[0], peers[0], DataFlags::IS_INTERNET)
                        }
                    }
                } else {
                    debug!(ip_version, "unsupported packet version from TUN; dropping");
                    continue;
                };

                debug!(%next_hop_id, "TUN pkt: routed; acquiring sessions lock");
                let session = state.sessions.read().await.get(&next_hop_id).cloned();
                debug!(%next_hop_id, has_session = session.is_some(), "TUN pkt: sessions lookup done");
                let Some(session) = session else {
                    debug!(%next_hop_id, "no session for next hop; dropping");
                    continue;
                };

                let payload: bytes::Bytes;

                // E2E-encrypt if we have the destination gateway's X25519 public key
                // and this packet is internet-bound. Must use the key of the *selected*
                // gateway (dst_id) — not some other gateway — or the gateway will fail
                // to decrypt.
                if flags.contains(DataFlags::IS_INTERNET) {
                    if let Some(gw_pub) = gw_x25519_by_id.get(&dst_id).copied() {
                        match e2e_encrypt(packet, &gw_pub) {
                            Ok(enc) => {
                                flags |= DataFlags::IS_E2E;
                                payload = bytes::Bytes::from(enc);
                            }
                            Err(e) => {
                                warn!("E2E encrypt failed: {e}");
                                payload = bytes::Bytes::copy_from_slice(packet);
                            }
                        }
                    } else {
                        payload = bytes::Bytes::copy_from_slice(packet);
                    }
                } else {
                    payload = bytes::Bytes::copy_from_slice(packet);
                }

                debug!(%dst_id, %next_hop_id, ?flags, bytes = payload.len(), "dispatching TUN packet to mesh");
                send_mesh_data(
                    &state,
                    &session,
                    state.self_id,
                    dst_id,
                    8,
                    flags,
                    &payload,
                )
                .await;
                debug!(%dst_id, "dispatched TUN packet");
                state.packets_forwarded.fetch_add(1, Ordering::Relaxed);
                state.bytes_forwarded.fetch_add(n as u64, Ordering::Relaxed);
            }

            // ── mesh → process ──────────────────────────────────────────────
            res = state.transport.recv() => {
                let (from_peer, frame) = match res {
                    Ok(v) => v,
                    Err(e) => { error!("transport recv: {e}"); break; }
                };

                // Rate-limit all incoming frames per peer.
                if !state.rate_limiter.lock().await.allow(&from_peer) {
                    debug!(%from_peer, "rate limit exceeded; dropping frame");
                    state.packets_dropped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                match frame.frame_type {
                    FrameType::Handshake => {
                        let wire = match decode_handshake_wire(&frame) {
                            Ok(w) => w,
                            Err(e) => { warn!(%from_peer, "bad hs frame: {e}"); continue; }
                        };
                        let tx = state.hs_channels.lock().await.get(&from_peer).cloned();
                        if let Some(tx) = tx {
                            tx.send(wire).await.ok();
                        } else if matches!(wire,
                            HandshakeWireFrame::InitOrResponse {
                                handshake_type: HandshakeFrameType::Init, ..
                            })
                        {
                            let remote_ip = state.transport.peer_addr(&from_peer).await.map(|addr| addr.ip());
                            let presented_peer_id = match &wire {
                                HandshakeWireFrame::InitOrResponse { sender_pub, .. } => {
                                    NodeId::from_public_key(sender_pub)
                                }
                                _ => unreachable!("matched only init frames above"),
                            };

                            if let Some(remote_ip) = remote_ip {
                                if let Some(pending) = state
                                    .pending_outbound
                                    .lock()
                                    .await
                                    .get(&remote_ip)
                                    .copied()
                                {
                                    if pending.transport_key != from_peer {
                                        if state.self_id.as_bytes() > presented_peer_id.as_bytes() {
                                            info!(
                                                %from_peer,
                                                %presented_peer_id,
                                                %remote_ip,
                                                winner = "incoming",
                                                "simultaneous connect detected; cancelling outbound attempt"
                                            );
                                            cancel_pending_outbound_for_ip(&state, remote_ip).await;
                                        } else {
                                            info!(
                                                %from_peer,
                                                %presented_peer_id,
                                                %remote_ip,
                                                winner = "outgoing",
                                                "simultaneous connect detected; dropping inbound attempt"
                                            );
                                            state.transport.disconnect(&from_peer).await.ok();
                                            continue;
                                        }
                                    }
                                }
                            }

                            // New incoming peer — spawn responder
                            let (tx, rx) = mpsc::channel(8);
                            state.hs_channels.lock().await.insert(from_peer, tx);
                            let st = state.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handshake_responder(&st, from_peer, wire, rx).await {
                                    warn!(%from_peer, "responder hs failed: {e}");
                                }
                                st.hs_channels.lock().await.remove(&from_peer);
                            });
                        } else {
                            warn!(%from_peer, "unexpected handshake frame (no pending hs)");
                        }
                    }

                    FrameType::Data => {
                        let session = state.sessions.read().await.get(&from_peer).cloned();
                        let Some(session) = session else {
                            warn!(%from_peer, "data frame before session; dropping");
                            continue;
                        };
                        let frame = match session.decrypt_frame(frame) {
                            Ok(f) => f,
                            Err(e) => { warn!(%from_peer, "decrypt: {e}"); continue; }
                        };
                        let mut buf = BytesMut::from(frame.payload.as_slice());
                        let mesh = match MeshDataFrame::decode(&mut buf) {
                            Ok(m) => m,
                            Err(e) => { warn!(%from_peer, "mesh decode: {e}"); continue; }
                        };

                        if mesh.dst_id == state.self_id {
                            if mesh.flags.contains(DataFlags::IS_CONTROL) {
                                let mut buf = BytesMut::from(mesh.payload.as_slice());
                                match ControlFrame::decode(&mut buf) {
                                    Ok(cf) => match cf {
                                        ControlFrame::IpRequest { requester_id } => {
                                            if let Some(pool) = &state.ip_pool {
                                                let disposition = {
                                                    let mut pending =
                                                        state.pending_ip_assignments.lock().await;
                                                    classify_ip_request(
                                                        &mut pending,
                                                        requester_id,
                                                        mesh.src_id,
                                                    )
                                                };
                                                match disposition {
                                                    IpRequestDisposition::SpoofedRequester => {
                                                        warn!(
                                                            src_id = %mesh.src_id,
                                                            %requester_id,
                                                            "rejecting routed IpRequest with mismatched requester_id"
                                                        );
                                                        continue;
                                                    }
                                                    IpRequestDisposition::DuplicateInFlight => {
                                                        debug!(
                                                            %requester_id,
                                                            "dropping duplicate in-flight routed IpRequest"
                                                        );
                                                        continue;
                                                    }
                                                    IpRequestDisposition::Process => {}
                                                }

                                                let result = pool
                                                    .lock()
                                                    .await
                                                    .allocate_assignment(*requester_id.as_bytes());
                                                match result {
                                                    Ok(assignment) => {
                                                        let sent = send_routed_control(
                                                            &state,
                                                            requester_id,
                                                            ControlFrame::IpAssign {
                                                                assigned_ip: assignment.assigned_ip.octets(),
                                                                subnet_mask: assignment.subnet_mask,
                                                                gateway_ip: assignment.gateway_ip.octets(),
                                                                lease_seconds: assignment.lease_seconds,
                                                            },
                                                        )
                                                        .await;
                                                        if sent {
                                                            info!(
                                                                %requester_id,
                                                                ip = %assignment.assigned_ip,
                                                                "assigned IP"
                                                            );
                                                        }
                                                    }
                                                    Err(e) => {
                                                        warn!(%requester_id, "IP allocation failed: {e}")
                                                    }
                                                }
                                                state
                                                    .pending_ip_assignments
                                                    .lock()
                                                    .await
                                                    .remove(&requester_id);
                                            } else {
                                                debug!(
                                                    src_id = %mesh.src_id,
                                                    "received routed IpRequest but not a gateway"
                                                );
                                            }
                                        }
                                        ControlFrame::IpAssign {
                                            assigned_ip,
                                            subnet_mask,
                                            gateway_ip,
                                            ..
                                        } => {
                                            if state.request_dynamic_ip {
                                                apply_dynamic_ip_assignment(
                                                    &state,
                                                    assigned_ip,
                                                    subnet_mask,
                                                    gateway_ip,
                                                )
                                                .await;
                                            } else {
                                                debug!(
                                                    src_id = %mesh.src_id,
                                                    "ignoring routed IpAssign for statically configured mesh IP"
                                                );
                                            }
                                        }
                                        other => {
                                            debug!(
                                                src_id = %mesh.src_id,
                                                frame = ?other,
                                                "ignoring unsupported routed control frame"
                                            );
                                        }
                                    },
                                    Err(e) => warn!(src_id = %mesh.src_id, "routed control decode: {e}"),
                                }
                                continue;
                            }

                            // Destined for us — count inbound data landing at this node
                            state.packets_forwarded.fetch_add(1, Ordering::Relaxed);
                            state.bytes_forwarded.fetch_add(mesh.payload.len() as u64, Ordering::Relaxed);
                            let ip_payload = match reassemble_or_deliver(
                                &state, mesh.src_id, mesh.flags, &mesh.payload,
                            ).await {
                                Some(p) => p,
                                None => continue, // waiting for more fragments
                            };

                            let mut ip_packet = ip_payload;

                            // E2E decrypt if gateway and flag is set
                            if state.is_gateway && mesh.flags.contains(DataFlags::IS_E2E) {
                                let seed = state.identity.signing_key().to_bytes();
                                if let Err(e) = e2e_decrypt_in_place(&mut ip_packet, &seed) {
                                        warn!(%from_peer, "E2E decrypt: {e}");
                                        continue;
                                }
                            }
                            let ip_packet_slice = ip_packet.as_slice();

                            // Check if the packet is destined for the gateway's
                            // own mesh IP.  We handle it in userspace instead of
                            // writing to TUN (where the reply would race with
                            // run_gateway_return / run_event_loop readers).
                            let dst_local = state.gw_engine.is_some()
                                && packet_ip_version(ip_packet_slice) == Some(4)
                                && ip_packet_slice.len() >= 20
                                && Ipv4Addr::new(
                                    ip_packet_slice[16], ip_packet_slice[17],
                                    ip_packet_slice[18], ip_packet_slice[19],
                                ) == Ipv4Addr::from(state.mesh_ip.load(Ordering::Relaxed));

                            if dst_local {
                                if let Some(reply) = icmp_echo_reply(ip_packet_slice) {
                                    let next_hop = state.routing.lock().await.lookup(mesh.src_id);
                                    let session = match next_hop {
                                        Some(next_hop) => state.sessions.read().await.get(&next_hop).cloned(),
                                        None => None,
                                    };
                                    if let Some(session) = session {
                                        send_mesh_data(
                                            &state, &session, state.self_id,
                                            mesh.src_id, 8, DataFlags::empty(),
                                            &reply,
                                        ).await;
                                    }
                                }
                            } else if mesh.flags.contains(DataFlags::IS_INTERNET) {
                                match packet_ip_version(ip_packet_slice) {
                                    Some(4) => match (&state.gw_engine, &state.internet_link) {
                                        (Some(gw), Some(link)) => {
                                            if let Err(e) = gw.translate_outbound(&mut ip_packet).await {
                                                warn!("gateway outbound NAT failed: {e}");
                                                continue;
                                            }
                                            if let Err(e) = link.send_packet(&ip_packet).await {
                                                warn!("gateway internet send failed: {e:#}");
                                            }
                                        }
                                        _ => {
                                            if let Err(e) = state.tun.write_packet(&ip_packet).await {
                                                warn!("TUN write: {e}");
                                            }
                                        }
                                    },
                                    Some(6) => {
                                        let gw_v6 = ensure_gateway_ipv6_engine(&state).await;
                                        match (gw_v6, &state.internet_link) {
                                            (Some(gw), Some(link)) => {
                                                if let Err(e) = gw.translate_outbound(&mut ip_packet, mesh.src_id).await {
                                                    warn!("gateway outbound NAT66 failed: {e}");
                                                    continue;
                                                }
                                                if let Err(e) = link.send_packet(&ip_packet).await {
                                                    warn!("gateway IPv6 internet send failed: {e:#}");
                                                }
                                            }
                                            _ => {
                                                warn!("IPv6 internet packet received but IPv6 gateway support is unavailable");
                                                continue;
                                            }
                                        }
                                    }
                                    _ => {
                                        warn!("unsupported internet-bound packet version");
                                        continue;
                                    }
                                }
                            } else {
                                if let Err(e) = state.tun.write_packet(&ip_packet).await {
                                    warn!("TUN write: {e}");
                                }
                            }
                        } else {
                            // Relay forwarding
                            if mesh.ttl == 0 {
                                debug!(%mesh.dst_id, "TTL expired; dropping");
                                state.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            let next = state
                                .routing
                                .lock()
                                .await
                                .lookup(mesh.dst_id);
                            let Some(next_hop) = next else {
                                debug!(%mesh.dst_id, "no route for relay; dropping");
                                state.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                continue;
                            };
                            let fwd_session =
                                state.sessions.read().await.get(&next_hop).cloned();
                            let Some(fwd_session) = fwd_session else {
                                warn!(%next_hop, "no session for relay next hop");
                                state.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                continue;
                            };
                            // Re-encrypt and forward with decremented TTL
                            send_single_mesh(
                                &state,
                                &fwd_session,
                                mesh.src_id,
                                mesh.dst_id,
                                mesh.ttl - 1,
                                mesh.flags,
                                &mesh.payload,
                            )
                            .await;
                            state.packets_forwarded.fetch_add(1, Ordering::Relaxed);
                            state.bytes_forwarded.fetch_add(mesh.payload.len() as u64, Ordering::Relaxed);
                            debug!(%mesh.src_id, %mesh.dst_id, via = %next_hop, "relayed");
                        }
                    }

                    FrameType::RouteUpdate => {
                        let mut buf = BytesMut::from(frame.payload.as_slice());
                        match RouteUpdateFrame::decode(&mut buf) {
                            Ok(update) => {
                                // Verify Ed25519 signature before applying.
                                let pub_bytes = state.peer_pubkeys.read().await
                                    .get(&from_peer).copied();
                                let allowed = match pub_bytes {
                                    Some(pk) => match VerifyingKey::from_bytes(&pk) {
                                        Ok(vk) => {
                                            if verify_route_update(&update, &vk) {
                                                true
                                            } else {
                                                warn!(%from_peer, "route update signature invalid; rejecting");
                                                false
                                            }
                                        }
                                        Err(e) => {
                                            warn!(%from_peer, "bad verifying key: {e}; rejecting route update");
                                            false
                                        }
                                    },
                                    None => {
                                        warn!(%from_peer, "no public key for peer; rejecting route update");
                                        false
                                    }
                                };
                                if allowed {
                                    let changed = state
                                        .routing
                                        .lock()
                                        .await
                                        .apply_update(&update, from_peer);
                                    debug!(%from_peer, ?changed, "route update applied");
                                    if changed == pim_routing::UpdateResult::Changed {
                                        maybe_request_dynamic_ip(&state).await;
                                    }
                                }
                            }
                            Err(e) => warn!(%from_peer, "route frame decode: {e}"),
                        }
                    }

                    FrameType::Heartbeat => {
                        let mut buf = BytesMut::from(frame.payload.as_slice());
                        match HeartbeatFrame::decode(&mut buf) {
                            Ok(hb) => {
                                debug!(%from_peer, gw_hops = hb.gateway_hops, load = hb.load, "heartbeat");
                                // Track liveness
                                state.peer_last_hb.lock().await.insert(from_peer, Instant::now());
                                // Direct gateway heartbeat: learn X25519 key and record load
                                if hb.gateway_hops == 0 {
                                    if hb.gw_x25519_pub != [0u8; 32] {
                                        gw_x25519_by_id.insert(from_peer, hb.gw_x25519_pub);
                                        debug!(%from_peer, "learned gateway X25519 pub key");
                                    }
                                    state.routing.lock().await.update_gateway_load(from_peer, hb.load);
                                }
                            }
                            Err(e) => warn!(%from_peer, "heartbeat decode: {e}"),
                        }
                    }

                    FrameType::Control => {
                        let mut buf = BytesMut::from(frame.payload.as_slice());
                        match ControlFrame::decode(&mut buf) {
                            Ok(cf) => match cf {
                                ControlFrame::IpRequest { requester_id } => {
                                    // Gateway allocates and responds
                                    if let Some(pool) = &state.ip_pool {
                                        let disposition = {
                                            let mut pending =
                                                state.pending_ip_assignments.lock().await;
                                            classify_ip_request(
                                                &mut pending,
                                                requester_id,
                                                from_peer,
                                            )
                                        };
                                        match disposition {
                                            IpRequestDisposition::SpoofedRequester => {
                                                warn!(
                                                    %from_peer,
                                                    %requester_id,
                                                    "rejecting IpRequest with mismatched requester_id"
                                                );
                                                continue;
                                            }
                                            IpRequestDisposition::DuplicateInFlight => {
                                                debug!(
                                                    %requester_id,
                                                    "dropping duplicate in-flight IpRequest"
                                                );
                                                continue;
                                            }
                                            IpRequestDisposition::Process => {}
                                        }

                                        let result = pool
                                            .lock()
                                            .await
                                            .allocate_assignment(*requester_id.as_bytes());
                                        match result {
                                            Ok(assignment) => {
                                                send_control(
                                                    &state,
                                                    &from_peer,
                                                    ControlFrame::IpAssign {
                                                        assigned_ip: assignment.assigned_ip.octets(),
                                                        subnet_mask: assignment.subnet_mask,
                                                        gateway_ip: assignment.gateway_ip.octets(),
                                                        lease_seconds: assignment.lease_seconds,
                                                    },
                                                )
                                                .await;
                                                info!(
                                                    %requester_id,
                                                    ip = %assignment.assigned_ip,
                                                    "assigned IP"
                                                );
                                            }
                                            Err(e) => warn!(%requester_id, "IP allocation failed: {e}"),
                                        }
                                        state
                                            .pending_ip_assignments
                                            .lock()
                                            .await
                                            .remove(&requester_id);
                                    } else {
                                        debug!(%from_peer, "received IpRequest but not a gateway");
                                    }
                                }
                                ControlFrame::IpAssign {
                                    assigned_ip,
                                    subnet_mask,
                                    gateway_ip,
                                    ..
                                } => {
                                    if state.request_dynamic_ip {
                                        apply_dynamic_ip_assignment(
                                            &state,
                                            assigned_ip,
                                            subnet_mask,
                                            gateway_ip,
                                        )
                                        .await;
                                    } else {
                                        debug!(%from_peer, "ignoring unsolicited IpAssign for statically configured mesh IP");
                                    }
                                }
                                ControlFrame::Goodbye { departing_id, .. } => {
                                    info!(%departing_id, "received Goodbye; removing peer");
                                    remove_peer(&state, departing_id).await;
                                }
                                ControlFrame::Ping { nonce } => {
                                    send_control(&state, &from_peer, ControlFrame::Pong { nonce }).await;
                                }
                                ControlFrame::Pong { nonce } => {
                                    if let Some((gw_id, sent_at)) =
                                        state.pending_pings.lock().await.remove(&nonce)
                                    {
                                        let rtt_ms = sent_at.elapsed().as_millis() as u32;
                                        state.routing.lock().await.update_gateway_rtt(gw_id, rtt_ms);
                                        debug!(%gw_id, rtt_ms, "gateway RTT measured via Pong");
                                        // Pong confirms peer is alive — positive reputation signal.
                                        state.reputation.lock().await.record_success(gw_id);
                                    }
                                }
                                ControlFrame::Rekey => {}
                            },
                            Err(e) => warn!(%from_peer, "control frame decode: {e}"),
                        }
                    }

                    _ => {
                        debug!(%from_peer, frame_type = ?frame.frame_type, "unhandled frame type");
                    }
                }
            }

            _ = state.cancel.cancelled() => {
                info!("event loop: shutdown signal");
                break;
            }
        }
    }

    // Graceful shutdown: send Goodbye to all peers
    let peers = state.transport.connected_peers();
    for peer in &peers {
        send_control(
            &state,
            peer,
            ControlFrame::Goodbye {
                departing_id: state.self_id,
                reason: 0,
            },
        )
        .await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await; // let Goodbye flush

    if !state.is_gateway {
        let mesh_ip = Ipv4Addr::from(state.mesh_ip.load(Ordering::Relaxed));
        let prefix_len = state.mesh_prefix_len.load(Ordering::Relaxed);
        let gateway_ip = first_host_in_subnet(mesh_ip, prefix_len);
        if let Err(e) = state.tun.remove_default_route(gateway_ip) {
            warn!(%gateway_ip, "default route cleanup failed: {e}");
        }
        if let Some((mesh_ipv6, prefix_len_v6)) = *state.mesh_ipv6.read().await {
            let gateway_ipv6 = first_host_in_subnet_v6(mesh_ipv6, prefix_len_v6);
            if let Err(e) = state.tun.remove_default_ipv6_route(gateway_ipv6) {
                warn!(%gateway_ipv6, "IPv6 default route cleanup failed: {e}");
            }
        }
    } else {
        // Gateway: reverse the iptables/ip6tables rules installed by
        // `setup_masquerade` so shutting pim down doesn't leave the host
        // dropping its own reply traffic.
        if let Some(gw) = state.gw_engine.as_ref() {
            let mesh_ip = Ipv4Addr::from(state.mesh_ip.load(Ordering::Relaxed));
            let prefix_len = state.mesh_prefix_len.load(Ordering::Relaxed);
            let cidr = format!("{mesh_ip}/{prefix_len}");
            gw.teardown_masquerade(&cidr);
        }
        if let Some(gw_v6) = state.gw_engine_v6.read().await.clone() {
            gw_v6.teardown_masquerade();
        }
    }

    // Clean up
    for peer in peers {
        state.transport.disconnect(&peer).await.ok();
    }
    // Release the TCP listening port: cancels accept loops and drops sockets.
    state.transport.shutdown().await;
    state.tun.down().ok();
    Ok(())
}

// ── Entry point ───────────────────────────────────────────────────────────────
