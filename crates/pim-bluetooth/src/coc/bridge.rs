//! L2CAP CoC ↔ local-TCP bridge.
//!
//! Byte-for-byte parallel to `rfcomm/bridge.rs`. After the
//! Hello/HelloAck handshake completes, bytes flow verbatim between the
//! post-handshake CoC channel and a freshly-opened loopback TCP
//! connection to the daemon's `pim-transport` listener. The peer's
//! 16-byte NodeId (learned during the handshake) is injected as the
//! first 16 bytes of the local TCP stream so `handle_incoming` reads
//! it as `peer_id` — never written back into the CoC stream because
//! that races the peer's own post-handshake reader.

#![cfg(target_os = "linux")]

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::socket::CocStream;

pub async fn run(
    coc: Arc<CocStream>,
    local_addr: SocketAddr,
    peer_label: String,
    peer_node_id: [u8; 16],
    cancel: CancellationToken,
) -> std::io::Result<()> {
    debug!(
        target: "pim-bluetooth-coc",
        peer = %peer_label,
        local_addr = %local_addr,
        "bridge: opening loopback TCP",
    );
    let tcp = TcpStream::connect(local_addr).await?;
    let _ = tcp.set_nodelay(true);
    let (mut tcp_r, mut tcp_w) = tcp.into_split();

    let coc_r = coc.clone();
    let coc_w = coc;

    if let Err(e) = tcp_w.write_all(&peer_node_id).await {
        warn!(
            target: "pim-bluetooth-coc",
            peer = %peer_label,
            err = %e,
            "bridge: failed to write peer NodeId prelude to loopback TCP",
        );
        return Err(e);
    }
    debug!(
        target: "pim-bluetooth-coc",
        peer = %peer_label,
        "bridge: wrote peer NodeId prelude (16 B) to loopback TCP",
    );

    let cancel_rt = cancel.clone();
    let label_rt = peer_label.clone();
    let r2t = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            tokio::select! {
                _ = cancel_rt.cancelled() => break "shutdown",
                r = coc_r.read(&mut buf) => match r {
                    Ok(0) => break "coc_eof",
                    Ok(n) => {
                        if let Err(e) = tcp_w.write_all(&buf[..n]).await {
                            warn!(
                                target: "pim-bluetooth-coc",
                                peer = %label_rt,
                                err = %e,
                                "bridge r→t: tcp write failed",
                            );
                            break "tcp_write_err";
                        }
                    }
                    Err(e) => {
                        warn!(
                            target: "pim-bluetooth-coc",
                            peer = %label_rt,
                            err = %e,
                            "bridge r→t: coc read failed",
                        );
                        break "coc_read_err";
                    }
                }
            }
        }
    });

    let cancel_tr = cancel.clone();
    let label_tr = peer_label.clone();
    let t2r = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            tokio::select! {
                _ = cancel_tr.cancelled() => break "shutdown",
                r = tcp_r.read(&mut buf) => match r {
                    Ok(0) => break "tcp_eof",
                    Ok(n) => {
                        if let Err(e) = coc_w.write_all(&buf[..n]).await {
                            warn!(
                                target: "pim-bluetooth-coc",
                                peer = %label_tr,
                                err = %e,
                                "bridge t→r: coc write failed",
                            );
                            break "coc_write_err";
                        }
                    }
                    Err(e) => {
                        warn!(
                            target: "pim-bluetooth-coc",
                            peer = %label_tr,
                            err = %e,
                            "bridge t→r: tcp read failed",
                        );
                        break "tcp_read_err";
                    }
                }
            }
        }
    });

    tokio::select! {
        r = r2t => {
            cancel.cancel();
            debug!(
                target: "pim-bluetooth-coc",
                peer = %peer_label,
                exit = ?r.ok(),
                "bridge r→t exited",
            );
        }
        r = t2r => {
            cancel.cancel();
            debug!(
                target: "pim-bluetooth-coc",
                peer = %peer_label,
                exit = ?r.ok(),
                "bridge t→r exited",
            );
        }
    }
    Ok(())
}
