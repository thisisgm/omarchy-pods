//! The enum numbers the status line publishes, read against the C++ enums they mirror, the Off option's one decision, what `mic` shows, and how the chain's counts fold.

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

#[test]
fn off_preference_keeps_stopping_visible() {
    let codes = [
        MicCode::Off,
        MicCode::Idle,
        MicCode::NotWorn,
        MicCode::Unsupported,
        MicCode::Starting,
        MicCode::Live,
        MicCode::Stopping,
        MicCode::Failed,
        MicCode::Ready,
        MicCode::Recording,
        MicCode::NoAudioServer,
        MicCode::LibraryError,
    ];
    for preference in MicPref::ALL {
        for code in codes {
            let mut microphone = Microphone::new(preference);
            microphone.show(code, 2, false);
            let shown = match (preference, code) {
                (_, MicCode::Stopping) => MicCode::Stopping,
                (MicPref::Off, _) => MicCode::Off,
                (_, MicCode::Off) => MicCode::Idle,
                (_, code) => code,
            };
            assert_eq!(microphone.code(), shown, "{preference:?} {code:?}");
            assert_eq!(
                microphone.preference(),
                preference,
                "{preference:?} {code:?}"
            );
            microphone.set_preference(MicPref::Always);
            assert_eq!(microphone.start_failures(), 2, "{preference:?} {code:?}");
        }
    }
}

#[test]
fn a_later_fold_of_the_counts_replaces_an_earlier_one() {
    let mut microphone = Microphone::new(MicPref::Off);
    microphone.fold(MicCounts {
        underruns: 1,
        concealed: 30,
        decode_errors: 1,
        limited: 30,
        nonfinite: 1,
    });
    microphone.fold(MicCounts {
        underruns: 30,
        concealed: 1,
        decode_errors: 30,
        limited: 1,
        nonfinite: 30,
    });
    // The tap's counters are running totals; each last value sits between the two before, so a sum, max or min misses it.
    let later = MicCounts {
        underruns: 15,
        concealed: 16,
        decode_errors: 17,
        limited: 18,
        nonfinite: 19,
    };
    microphone.fold(later);
    assert_eq!(microphone.counts(), later);
}
