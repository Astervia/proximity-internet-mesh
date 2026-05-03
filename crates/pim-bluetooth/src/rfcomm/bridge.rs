//! RFCOMM ↔ local-TCP bridge.
//!
//! After the Hello/HelloAck handshake completes, the post-handshake
//! channel is repurposed as a byte-pipe to the daemon's existing
//! TCP transport listener. Bytes flow verbatim in both directions —
//! the daemon's `TcpTransport::handle_incoming` reads the first 16 B
//! as the peer NodeId and treats the rest as length-delimited
//! `pim-protocol` frames, exactly as if a normal TCP peer had
//! connected.
//!
//! Wire choreography: each side's bridge dials `127.0.0.1:9100` and
//! writes the *peer's* 16-byte NodeId (learned from the RFCOMM
//! Hello/HelloAck) directly into that local TCP socket — never into
//! the RFCOMM stream. The local listener consumes those 16 bytes as
//! `peer_id` in `handle_incoming`. Doing it this way (rather than
//! sending self_node_id over RFCOMM) avoids a race where the peer's
//! `session::handshake` reader sees post-handshake binary bytes as
//! a length-prefixed frame and chokes with
//! `frame size 246322734 exceeds max 65536`.
//!
//! This avoids refactoring the entire `pim-transport` layer to
//! understand RFCOMM as a transport: from the daemon's point of
//! view, an RFCOMM-discovered peer is indistinguishable from a TCP
//! peer reachable on `127.0.0.1`.

#![cfg(target_os = "linux")]

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::socket::RfcommStream;

/// Bridge bytes between an open RFCOMM stream and a freshly-opened
/// TCP loopback connection to `local_addr`. Returns when either side
/// closes or `cancel` fires.
///
/// `peer_node_id` is the REMOTE peer's 16-byte NodeId, learned from
/// the RFCOMM Hello/HelloAck. It is written to the local TCP socket
/// as the very first 16 bytes so the daemon's `handle_incoming`
/// identifies the bridged peer correctly.
/// `peer_label` is used only in log lines so operators can correlate
/// kernel logs with the discovery `bd_addr` of the originating peer.
pub async fn run(
    rfcomm: Arc<RfcommStream>,
    local_addr: SocketAddr,
    peer_label: String,
    peer_node_id: [u8; 16],
    cancel: CancellationToken,
) -> std::io::Result<()> {
    debug!(
        target: "pim-bluetooth-rfcomm",
        peer = %peer_label,
        local_addr = %local_addr,
        "bridge: opening loopback TCP",
    );
    let tcp = TcpStream::connect(local_addr).await?;
    // Disable Nagle so single-frame mesh control frames don't sit in
    // the kernel buffer waiting for a follow-up frame; the RFCOMM
    // channel is already MTU-bounded, no point coalescing.
    let _ = tcp.set_nodelay(true);
    let (mut tcp_r, mut tcp_w) = tcp.into_split();

    let rfcomm_r = rfcomm.clone();
    let rfcomm_w = rfcomm;

    // Inject the peer's NodeId as the first 16 bytes of the local
    // TCP stream so `handle_incoming` consumes it as `peer_id`. We
    // intentionally do NOT write into the RFCOMM stream here — that
    // path collides with the peer's `session::handshake` reader if
    // the peer hasn't yet transitioned to the bridge phase, and
    // produces the spurious `frame size 246322734 exceeds max 65536`
    // teardowns we hit on the linux↔linux bench.
    if let Err(e) = tcp_w.write_all(&peer_node_id).await {
        warn!(
            target: "pim-bluetooth-rfcomm",
            peer = %peer_label,
            err = %e,
            "bridge: failed to write peer NodeId prelude to loopback TCP",
        );
        return Err(e);
    }
    debug!(
        target: "pim-bluetooth-rfcomm",
        peer = %peer_label,
        "bridge: wrote peer NodeId prelude (16 B) to loopback TCP",
    );

    // RFCOMM → TCP
    let cancel_rt = cancel.clone();
    let label_rt = peer_label.clone();
    let r2t = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            tokio::select! {
                _ = cancel_rt.cancelled() => break "shutdown",
                r = rfcomm_r.read(&mut buf) => match r {
                    Ok(0) => break "rfcomm_eof",
                    Ok(n) => {
                        if let Err(e) = tcp_w.write_all(&buf[..n]).await {
                            warn!(
                                target: "pim-bluetooth-rfcomm",
                                peer = %label_rt,
                                err = %e,
                                "bridge r→t: tcp write failed",
                            );
                            break "tcp_write_err";
                        }
                    }
                    Err(e) => {
                        warn!(
                            target: "pim-bluetooth-rfcomm",
                            peer = %label_rt,
                            err = %e,
                            "bridge r→t: rfcomm read failed",
                        );
                        break "rfcomm_read_err";
                    }
                }
            }
        }
    });

    // TCP → RFCOMM
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
                        if let Err(e) = rfcomm_w.write_all(&buf[..n]).await {
                            warn!(
                                target: "pim-bluetooth-rfcomm",
                                peer = %label_tr,
                                err = %e,
                                "bridge t→r: rfcomm write failed",
                            );
                            break "rfcomm_write_err";
                        }
                    }
                    Err(e) => {
                        warn!(
                            target: "pim-bluetooth-rfcomm",
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

    // First half-duplex closure cancels the other so both tasks unwind.
    tokio::select! {
        r = r2t => {
            cancel.cancel();
            debug!(
                target: "pim-bluetooth-rfcomm",
                peer = %peer_label,
                exit = ?r.ok(),
                "bridge r→t exited",
            );
        }
        r = t2r => {
            cancel.cancel();
            debug!(
                target: "pim-bluetooth-rfcomm",
                peer = %peer_label,
                exit = ?r.ok(),
                "bridge t→r exited",
            );
        }
    }
    Ok(())
}
