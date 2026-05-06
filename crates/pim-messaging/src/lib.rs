//! User-to-user encrypted messaging — `pim-daemon` plugin.
//!
//! This crate is loaded by `pim-daemon` only when the `messaging`
//! Cargo feature is enabled. It provides:
//!
//! - On-disk persistence of message history + conversation summaries
//!   ([`storage::MessagingStorage`]), separate from the daemon-owned
//!   peer keystore.
//! - End-to-end encryption of message bodies via `pim-crypto`'s ECIES
//!   helpers.
//! - On-wire encoding of `messaging.msg` and `messaging.ack` payloads
//!   carried inside [`pim_protocol::ControlFrame::PluginPayload`]
//!   (see [`wire`]).
//! - A [`pim_plugin::DaemonPlugin`] implementation
//!   ([`plugin::MessagingPlugin`]) that the daemon registers in its
//!   plugin list when the `messaging` feature is on.

#![warn(missing_docs)]

pub mod plugin;
pub mod service;
pub mod storage;
pub mod wire;

pub use plugin::MessagingPlugin;
pub use service::{HistoryScope, MessageEvent, MessagingService, MAX_BODY_BYTES};
pub use storage::{
    AckKind, ConversationSummary, MessageDirection, MessageRecord, MessageStatus, MessagingStorage,
};
pub use wire::{KIND_ACK, KIND_MESSAGE};

use pim_core::NodeId;

/// Format a [`NodeId`] as a 32-char lowercase hex string.
pub fn hex_node_id(id: &NodeId) -> String {
    let mut out = String::with_capacity(32);
    for b in id.as_bytes() {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// Format a 16-byte UUID as a 32-char lowercase hex string.
pub fn hex16(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(32);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}
