use super::*;

mod convergence;
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

fn mesh_ip(n: u8) -> [u8; 4] {
    [10, 77, 0, n]
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
