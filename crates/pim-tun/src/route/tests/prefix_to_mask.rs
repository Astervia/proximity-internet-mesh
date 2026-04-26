use super::super::*;

#[test]
pub(crate) fn prefix_to_mask_standard() {
    assert_eq!(prefix_to_mask(24), Ipv4Addr::new(255, 255, 255, 0));
    assert_eq!(prefix_to_mask(16), Ipv4Addr::new(255, 255, 0, 0));
    assert_eq!(prefix_to_mask(8), Ipv4Addr::new(255, 0, 0, 0));
    assert_eq!(prefix_to_mask(30), Ipv4Addr::new(255, 255, 255, 252));
    assert_eq!(prefix_to_mask(0), Ipv4Addr::new(0, 0, 0, 0));
    assert_eq!(prefix_to_mask(32), Ipv4Addr::new(255, 255, 255, 255));
}
