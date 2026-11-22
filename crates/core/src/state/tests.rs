//! The enum numbers the status line publishes, read against the C++ enums they mirror, and the Off option's one decision.

use cpp_source::{BLE_MANAGER, MEDIA_CONTROLLER};

use super::*;

#[test]
fn the_pods_report_of_the_off_option_wins_over_the_model_table() {
    for (model, report, offered) in [
        (Model::AirPodsPro3, None, false),
        (Model::AirPodsPro3, Some(true), true),
        (Model::AirPodsPro2USBC, None, true),
        (Model::AirPodsPro2USBC, Some(false), false),
    ] {
        let state = State {
            model,
            noise_off: report,
            ..State::default()
        };
        assert_eq!(state.supports_noise_off(), offered, "{model:?} {report:?}");
        assert_eq!(
            state.capabilities(),
            Capabilities {
                noise_off: offered,
                ..model.capabilities()
            }
        );
    }
}

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
