//! The enum numbers the status line publishes, the Off option's one decision, what `mic` shows, and how the chain's counts fold.

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
fn either_primary_bud_keeps_the_same_mic_node_name() {
    for (reported, node) in [
        ("A3063", "pods_input.a3063"),
        ("A3064", "pods_input.a3063"),
        ("Z0000", "pods_input.z0000"),
    ] {
        let state = State {
            model: Model::from_number(reported),
            model_number: reported.to_owned(),
            ..State::default()
        };
        assert_eq!(state.mic_node_name(), node, "reported {reported}");
        assert_eq!(state.model_number, reported);
    }
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
