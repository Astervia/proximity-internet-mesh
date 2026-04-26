use super::super::*;

#[test]
fn should_buffer_under_congestion_control_frames() {
    assert!(
        should_buffer_under_congestion(FrameType::Control),
        "control frames must be buffered under congestion"
    );
}

#[test]
fn should_buffer_under_congestion_route_frames() {
    assert!(
        should_buffer_under_congestion(FrameType::RouteUpdate),
        "route-update frames must be buffered under congestion"
    );
}

#[test]
fn should_buffer_under_congestion_handshake_frames() {
    assert!(
        should_buffer_under_congestion(FrameType::Handshake),
        "handshake frames must be buffered under congestion"
    );
}

#[test]
fn should_drop_data_frames_under_congestion() {
    assert!(
        !should_buffer_under_congestion(FrameType::Data),
        "data frames must be dropped (not buffered) under congestion"
    );
}

#[test]
fn should_drop_heartbeat_frames_under_congestion() {
    assert!(
        !should_buffer_under_congestion(FrameType::Heartbeat),
        "heartbeat frames must be dropped (not buffered) under congestion"
    );
}

#[test]
fn congestion_drop_policy_is_priority_based() {
    // Only Data-priority frame types are dropped; everything more important is buffered.
    use pim_protocol::FrameType;
    let high_priority = [
        FrameType::Control,
        FrameType::Handshake,
        FrameType::RouteUpdate,
    ];
    let low_priority = [FrameType::Data, FrameType::Heartbeat];
    for ft in high_priority {
        assert!(
            should_buffer_under_congestion(ft),
            "{ft:?} should be buffered"
        );
    }
    for ft in low_priority {
        assert!(
            !should_buffer_under_congestion(ft),
            "{ft:?} should be dropped"
        );
    }
}
