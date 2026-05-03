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
//! Wire choreography: each side's bridge writes the LOCAL node's
//! 16-byte NodeId to the RFCOMM stream as soon as the bridge opens.
//! Those bytes flow over RFCOMM to the *peer's* bridge, which pumps
//! them to the peer's `127.0.0.1:9100` listener — where they are
//! consumed as the (us-shaped) peer NodeId by `handle_incoming`. So
//! both sides' listeners observe the correct peer NodeId, even
//! though both bridges connect AS the TCP-dialer to their own
//! loopback. Without this injection both listeners block forever
//! waiting for that 16-byte prefix.
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
/// `self_node_id` is the LOCAL daemon's 16-byte NodeId — written into
/// the RFCOMM stream as the very first bytes after the bridge opens
/// so the *peer's* `handle_incoming` can consume it as `peer_id`.
/// `peer_label` is used only in log lines so operators can correlate
/// kernel logs with the discovery `bd_addr` of the originating peer.
pub async fn run(
    rfcomm: Arc<RfcommStream>,
    local_addr: SocketAddr,
    peer_label: String,
    self_node_id: [u8; 16],
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

    // Inject our own NodeId as the first 16 bytes of the RFCOMM
    // stream. The peer's bridge will pump these bytes to its own
    // loopback TCP, where the daemon's `handle_incoming` consumes
    // them as the dialing peer's NodeId. Both sides do this
    // symmetrically.
    if let Err(e) = rfcomm_w.write_all(&self_node_id).await {
        warn!(
            target: "pim-bluetooth-rfcomm",
            peer = %peer_label,
            err = %e,
            "bridge: failed to write self NodeId prelude",
        );
        return Err(e);
    }
    debug!(
        target: "pim-bluetooth-rfcomm",
        peer = %peer_label,
        "bridge: wrote self NodeId prelude (16 B)",
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
