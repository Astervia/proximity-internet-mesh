use super::super::*;
use std::path::{Path, PathBuf};

#[test]
fn macos_auto_interface_hint_defaults_to_bridge0() {
    assert_eq!(resolve_macos_pan_interface_hint("auto"), "bridge0");
    assert_eq!(resolve_macos_pan_interface_hint(""), "bridge0");
    assert_eq!(resolve_macos_pan_interface_hint("bridge1"), "bridge1");
}
#[test]
fn interface_operstate_path_uses_supplied_root() {
    let path = interface_operstate_path(Path::new("/tmp/fake-sysfs"), "bnep0");
    assert_eq!(path, PathBuf::from("/tmp/fake-sysfs/bnep0/operstate"));
}

#[test]
fn preferred_interface_hint_treats_auto_as_unset() {
    assert_eq!(preferred_interface_hint("auto"), None);
    assert_eq!(preferred_interface_hint(""), None);
    assert_eq!(preferred_interface_hint("bnep0"), Some("bnep0"));
}

#[test]
fn select_pan_interfaces_prefers_configured_ready_interface() {
    let selected = select_pan_interfaces(
        &[
            PanInterfaceCandidate {
                name: "enx1234".into(),
                operstate: Some("up".into()),
            },
            PanInterfaceCandidate {
                name: "bnep7".into(),
                operstate: Some("up".into()),
            },
        ],
        Some("enx1234"),
        None,
    );
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "enx1234");
    assert_eq!(selected[0].source, "configured");
}

#[test]
fn select_pan_interfaces_fall_back_to_dynamic_linux_names() {
    let selected = select_pan_interfaces(
        &[
            PanInterfaceCandidate {
                name: "eth0".into(),
                operstate: Some("up".into()),
            },
            PanInterfaceCandidate {
                name: "enx6432a8144f4b".into(),
                operstate: Some("up".into()),
            },
        ],
        Some("bnep0"),
        None,
    );
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "enx6432a8144f4b");
    assert_eq!(selected[0].source, "dynamic-enx");
}

#[test]
fn select_pan_interfaces_use_nap_bridge_when_serving() {
    let selected = select_pan_interfaces(
        &[PanInterfaceCandidate {
            name: "br-bt".into(),
            operstate: Some("down".into()),
        }],
        None,
        Some("br-bt"),
    );
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "br-bt");
    assert_eq!(selected[0].source, "nap_bridge");
}

#[test]
fn select_pan_interfaces_include_all_ready_dynamic_pan_links() {
    let selected = select_pan_interfaces(
        &[
            PanInterfaceCandidate {
                name: "bnep0".into(),
                operstate: Some("up".into()),
            },
            PanInterfaceCandidate {
                name: "enx6432a8144f4b".into(),
                operstate: Some("up".into()),
            },
            PanInterfaceCandidate {
                name: "eth0".into(),
                operstate: Some("up".into()),
            },
        ],
        None,
        None,
    );
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].name, "bnep0");
    assert_eq!(selected[1].name, "enx6432a8144f4b");
}
