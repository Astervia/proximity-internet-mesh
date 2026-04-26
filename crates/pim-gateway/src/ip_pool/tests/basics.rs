use super::super::*;

fn pool() -> IpPool {
    IpPool::new(Ipv4Addr::new(10, 77, 0, 0), 24)
}

#[test]
fn first_allocation_skips_gateway_ip() {
    let mut p = pool();
    let (ip, _) = p.allocate([1u8; 16]).unwrap();
    // offset 1 = 10.77.0.1 (gateway, reserved) → first client gets .2
    assert_eq!(ip, Ipv4Addr::new(10, 77, 0, 2));
}

#[test]
fn sequential_allocations_get_different_ips() {
    let mut p = pool();
    let (ip1, _) = p.allocate([1u8; 16]).unwrap();
    let (ip2, _) = p.allocate([2u8; 16]).unwrap();
    assert_ne!(ip1, ip2);
}

#[test]
fn same_node_gets_same_ip() {
    let mut p = pool();
    let id = [42u8; 16];
    let (ip1, _) = p.allocate(id).unwrap();
    let (ip2, _) = p.allocate(id).unwrap();
    assert_eq!(ip1, ip2);
}

#[test]
fn release_frees_ip_for_reuse() {
    let mut p = pool();
    let id1 = [1u8; 16];
    let (ip1, _) = p.allocate(id1).unwrap();
    p.release(&id1);

    // Next allocation may reuse the freed slot
    let id2 = [2u8; 16];
    p.allocate(id2).unwrap();
    assert!(p.get_lease(&id1).is_none());
    assert_eq!(p.len(), 1);
    let _ = ip1;
}

#[test]
fn pool_exhaustion_returns_error() {
    // /30 has 2 usable addresses (offset 2 and 3), minus gateway = 2 clients max... actually
    // /30: 4 addresses total. offset 0=network, 1=gateway, 2=client1, 3=broadcast
    // So only 1 client can be assigned.
    let mut p = IpPool::new(Ipv4Addr::new(10, 0, 0, 0), 30);
    p.allocate([1u8; 16]).unwrap(); // offset 2
                                    // offset 3 = broadcast — pool exhausted for clients
    let result = p.allocate([2u8; 16]);
    assert!(result.is_err(), "pool should be exhausted");
}

#[test]
fn gateway_ip_is_offset_1() {
    let p = pool();
    assert_eq!(p.gateway_ip(), Ipv4Addr::new(10, 77, 0, 1));
}

#[test]
fn expired_lease_can_be_reallocated() {
    let mut p = IpPool::new(Ipv4Addr::new(10, 0, 0, 0), 30).with_lease_duration(Duration::ZERO);
    p.allocate([1u8; 16]).unwrap();
    // Lease immediately expired — should be able to allocate for a different node
    let result = p.allocate([2u8; 16]);
    assert!(result.is_ok());
}

#[test]
fn allocate_assignment_returns_consistent_network_snapshot() {
    let mut p = pool();
    let assignment = p.allocate_assignment([7u8; 16]).unwrap();
    assert_eq!(assignment.assigned_ip, Ipv4Addr::new(10, 77, 0, 2));
    assert_eq!(assignment.gateway_ip, Ipv4Addr::new(10, 77, 0, 1));
    assert_eq!(assignment.subnet_mask, 24);
    assert_eq!(assignment.lease_seconds, 3600);
}

#[test]
fn len_tracks_allocations() {
    let mut p = pool();
    assert_eq!(p.len(), 0);
    p.allocate([1u8; 16]).unwrap();
    p.allocate([2u8; 16]).unwrap();
    assert_eq!(p.len(), 2);
    p.release(&[1u8; 16]);
    assert_eq!(p.len(), 1);
}
