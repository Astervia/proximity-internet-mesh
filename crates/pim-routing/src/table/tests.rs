use super::*;

mod convergence;
mod derivation;
mod gateway_selection;
mod invalidation;
mod multi_gateway;
mod security;
mod sequence;
mod split_horizon;
mod stale_expiry;

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

/// Synthetic mesh IP previously used to seed RouteUpdate frames.
/// Now stored only in advertisements (the routing table derives the
/// trusted value from the destination NodeId on insert).
fn mesh_ip(n: u8) -> [u8; 4] {
    [10, 77, 0, n]
}

/// Test mesh prefix. Small `/24` keeps the host space tractable for
/// reverse-lookup assertions without the docs-RFC `192.0.2.0/24`
/// triggering tooling that special-cases TEST-NET addresses.
fn test_prefix() -> Ipv4Prefix {
    Ipv4Prefix::parse("10.77.0.0/24").expect("valid test prefix")
}

/// Convenience constructor mirroring the previous two-arg signature.
fn new_table(self_id: NodeId, is_gateway: bool) -> RoutingTable {
    RoutingTable::new(self_id, is_gateway, test_prefix())
}

/// Build a RouteUpdateFrame advertising `entries` from `origin`.
fn advertisement(origin: NodeId, seq: u64, entries: Vec<(NodeId, u8, bool)>) -> RouteUpdateFrame {
    RouteUpdateFrame {
        origin_id: origin,
        sequence: seq,
        entries: entries
            .into_iter()
            .map(|(dst, hops, is_gw)| RouteEntry {
                destination: dst,
                hops,
                flags: if is_gw { 0x01 } else { 0x00 },
                mesh_ip: mesh_ip(dst.as_bytes()[0]),
            })
            .collect(),
        signature: [0u8; 64],
    }
}
