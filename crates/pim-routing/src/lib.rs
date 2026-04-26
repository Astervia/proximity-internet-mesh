//! Distance-vector routing engine for the proximity mesh.
//!
//! Maintains next-hop reachability, gateway selection, and replay-protected
//! route advertisement processing for the local node.

#![warn(missing_docs)]

pub mod signing;
mod table;

pub use table::{gateway_score, RouteTableEntry, RoutingTable, UpdateResult, INFINITY};
