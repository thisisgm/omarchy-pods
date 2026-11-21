//! The enum numbers the status line publishes, read against the C++ enums they mirror.

use cpp_source::{BLE_MANAGER, MEDIA_CONTROLLER};

use super::*;

#[test]
fn ear_detection_behavior_numbers_match_the_cpp_enum() {
    let rust: Vec<(&str, u8)> = [
        (
            "PauseWhenOneRemoved",
            EarDetectionBehavior::PauseWhenOneRemoved,
        ),
        (
            "PauseWhenBothRemoved",
            EarDetectionBehavior::PauseWhenBothRemoved,
        ),
        ("Disabled", EarDetectionBehavior::Disabled),
    ]
    .into_iter()
    .map(|(name, behavior)| (name, behavior.number()))
    .collect();
    assert_eq!(
        rust,
        MEDIA_CONTROLLER.enumerators("enum EarDetectionBehavior")
    );
}

#[test]
fn lid_state_numbers_match_the_cpp_enum() {
    let rust: Vec<(&str, u8)> = [
        ("OPEN", LidState::Open),
        ("CLOSED", LidState::Closed),
        ("UNKNOWN", LidState::Unknown),
    ]
    .into_iter()
    .map(|(name, lid)| (name, lid.number()))
    .collect();
    assert_eq!(rust, BLE_MANAGER.enumerators("enum class LidState"));
}
