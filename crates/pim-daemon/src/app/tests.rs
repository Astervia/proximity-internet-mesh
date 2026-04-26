use super::*;

mod auth;
mod backoff;
mod bluetooth;
mod capabilities;
mod discovery_integration;
mod flow_control;
mod gateway;
mod interface;
mod observability;
mod peer_lifecycle;
mod reconnect;
mod wifi_direct;

fn peer_id(b: u8) -> NodeId {
    NodeId::from_bytes([b; 16])
}
