//! Every state field lands on its own key with the C++ value.

use pods_proto::battery::{Batteries, Charge, Component};
use pods_proto::ear::{EarPlacement, EarReport};
use pods_proto::model::Model;
use pods_proto::noise::NoiseMode;
use serde_json::{Value, json};

use super::*;
use crate::command::tests::level;
use crate::prefs::{MicModels, MicPref};
use crate::state::{
    AdvertIntake, AudioServer, Counters, EarDetectionBehavior, LidState, MicCode, MicCounts,
    MicDictation, MicSpan, Microphone,
};

fn status(state: &State) -> Value {
    serde_json::from_str(&to_status_line(state)).unwrap()
}

fn charge(level: u8, charging: bool) -> Charge {
    Charge { level, charging }
}

/// Five counts apart, so a key that reads the wrong count shows.
const FOLDED: MicCounts = MicCounts {
    underruns: 15,
    concealed: 16,
    decode_errors: 17,
    limited: 18,
    nonfinite: 19,
};

fn populated_state() -> State {
    let mut microphone = Microphone::new(MicPref::Off);
    microphone.show(MicCode::Off, 14, true);
    microphone.fold(FOLDED);
    microphone.fold_span(MicSpan::new(20, 21, 22, 0.5));
    State {
        connected: true,
        device_name: "Desk pods".to_owned(),
        model: Model::AirPodsPro2USBC,
        model_number: "A3048".to_owned(),
        noise_mode: NoiseMode::Adaptive,
        noise_off: Some(false),
        adaptive_noise_level: level(37),
        conversational_awareness: true,
        one_bud_anc_mode: true,
        batteries: Batteries {
            left: Some(charge(80, true)),
            right: Some(charge(75, false)),
            case: Some(charge(40, true)),
            headset: None,
        },
        ear: EarReport {
            primary: EarPlacement::InEar,
            secondary: EarPlacement::NotInEar,
        },
        primary: Some(Component::Right),
        ear_detection_behavior: EarDetectionBehavior::Disabled,
        lid_state: LidState::Closed,
        counters: Counters {
            reconnect_attempts: 1,
            reconnect_failures: 2,
            noise_control_changes: 3,
            forget_calls: 4,
            ear_detection_changes: 5,
            conversational_awareness_changes: 6,
            disconnect_calls: 7,
            connect_calls: 8,
            disconnect_failures: 9,
            connect_failures: 10,
            adaptive_level_changes: 11,
            one_bud_anc_changes: 12,
            reopen_calls: 13,
        },
        notifications: false,
        microphone,
        mic_dictation: MicDictation::Armed,
        mic_models: MicModels::default(),
        audio_server: AudioServer::Up,
        advert_intake: AdvertIntake::Listening,
    }
}

#[test]
fn populated_state_lands_every_field_on_its_key() {
    let mut expected = json!({
        "adaptive_level_changes_total": 11,
        "adaptive_noise_level": 37,
        "advert_intake": "listening",
        "audio_server": "up",
        "ca_changes_total": 6,
        "case": {"available": true, "charging": true, "level": 40},
        "connect_calls_total": 8,
        "connect_failures_total": 10,
        "connected": true,
        "conversational_awareness": true,
        "device_name": "Desk pods",
        "disconnect_calls_total": 7,
        "disconnect_failures_total": 9,
        "ear_detection_behavior": 2,
        "ear_detection_changes_total": 5,
        "forget_calls_total": 4,
        "headset": {"available": false, "charging": false, "level": 0},
        "is_headset": false,
        "is_pro_series": true,
        "left": {"available": true, "charging": true, "in_ear": false, "level": 80},
        "lid_state": 1,
        "mic": "off",
        "microphone": "off",
        "model_int": 6,
        "model_name": "AirPods Pro 2 (USB-C)",
        "model_number": "A3048",
        "noise_control_changes_total": 3,
        "noise_mode": 3,
        "notifications_enabled": false,
        "one_bud_anc_changes_total": 12,
        "one_bud_anc_mode": true,
        "reconnect_attempts_total": 1,
        "reconnect_failures_total": 2,
        "reopen_calls_total": 13,
        "right": {"available": true, "charging": false, "in_ear": true, "level": 75},
        "schema_version": 1,
        "supports_adaptive": true,
        "supports_conversational_awareness": true,
        "supports_noise_control": true,
        "supports_noise_off": false,
        "supports_one_bud_anc": true,
    });
    // Past the json macro's recursion limit.
    expected["mic_dictation"] = json!("armed");
    expected["mic_start_failures"] = json!(14);
    expected["mic_source"] = json!("omarchypods_input.a3048");
    expected["mic_underruns"] = json!(15);
    expected["mic_concealed"] = json!(16);
    expected["mic_decode_errors"] = json!(17);
    expected["mic_limited"] = json!(18);
    expected["mic_nonfinite"] = json!(19);
    expected["mic_span_sdus"] = json!(20);
    expected["mic_span_restarts"] = json!(21);
    expected["mic_span_gone_frames"] = json!(22);
    expected["mic_span_peak_dbfs"] = json!(-6.0);
    assert_eq!(status(&populated_state()), expected);
}

#[test]
fn a_side_that_is_not_primary_reads_the_secondary_placement() {
    let ear = EarReport {
        primary: EarPlacement::InEar,
        secondary: EarPlacement::NotInEar,
    };
    for (primary, left, right) in [
        (Some(Component::Left), true, false),
        (Some(Component::Right), false, true),
        (None, false, false),
    ] {
        let line = status(&State {
            ear,
            primary,
            ..State::default()
        });
        assert_eq!(line["left"]["in_ear"], Value::Bool(left), "{primary:?}");
        assert_eq!(line["right"]["in_ear"], Value::Bool(right), "{primary:?}");
    }
}

#[test]
fn headset_battery_and_capabilities_land_on_their_keys() {
    let line = status(&State {
        model: Model::AirPodsMaxLightning,
        batteries: Batteries {
            headset: Some(charge(64, true)),
            ..Batteries::default()
        },
        ..State::default()
    });
    assert_eq!(
        line["headset"],
        json!({"available": true, "charging": true, "level": 64})
    );
    assert_eq!(line["is_headset"], Value::Bool(true));
    assert_eq!(line["is_pro_series"], Value::Bool(false));
    assert_eq!(line["supports_one_bud_anc"], Value::Bool(false));
    assert_eq!(line["supports_adaptive"], Value::Bool(false));
    assert_eq!(line["model_name"], Value::from("AirPods Max"));
    assert_eq!(line["model_int"], Value::from(7));
    let pro3 = status(&State {
        model: Model::AirPodsPro3,
        ..State::default()
    });
    assert_eq!(pro3["supports_noise_off"], Value::Bool(false));
    let plain = status(&State {
        model: Model::AirPods4,
        ..State::default()
    });
    assert_eq!(plain["supports_noise_control"], Value::Bool(false));
}

#[test]
fn every_noise_mode_ear_detection_behavior_and_lid_state_lands_on_its_key() {
    for mode in [
        NoiseMode::Off,
        NoiseMode::Anc,
        NoiseMode::Transparency,
        NoiseMode::Adaptive,
    ] {
        let line = status(&State {
            noise_mode: mode,
            ..State::default()
        });
        assert_eq!(line["noise_mode"], Value::from(u8::from(mode)), "{mode:?}");
    }
    for behavior in [
        EarDetectionBehavior::PauseWhenOneRemoved,
        EarDetectionBehavior::PauseWhenBothRemoved,
        EarDetectionBehavior::Disabled,
    ] {
        let line = status(&State {
            ear_detection_behavior: behavior,
            ..State::default()
        });
        assert_eq!(
            line["ear_detection_behavior"],
            Value::from(behavior.number()),
            "{behavior:?}"
        );
    }
    for lid in [LidState::Open, LidState::Closed, LidState::Unknown] {
        let line = status(&State {
            lid_state: lid,
            ..State::default()
        });
        assert_eq!(line["lid_state"], Value::from(lid.number()), "{lid:?}");
    }
}

#[test]
fn the_document_has_the_cpp_keys_and_each_additive_key_once() {
    let document = status(&State::default());
    let keys = document.as_object().unwrap();
    assert_eq!(
        keys.len(),
        CPP_KEY_COUNT + ADDITIVE_KEYS.len(),
        "{document}"
    );
    for key in ADDITIVE_KEYS {
        assert!(keys.contains_key(*key), "{key}");
    }
}

#[test]
fn every_audio_server_word_lands_on_its_key() {
    for (server, word) in [
        (AudioServer::Idle, "idle"),
        (AudioServer::Up, "up"),
        (AudioServer::Down, "down"),
    ] {
        let line = status(&State {
            audio_server: server,
            ..State::default()
        });
        assert_eq!(line["audio_server"], Value::from(word), "{server:?}");
    }
}

#[test]
fn every_microphone_word_lands_on_its_key() {
    for preference in MicPref::ALL {
        let mic = match preference {
            MicPref::Off => "off",
            MicPref::Auto | MicPref::Always => "idle",
        };
        let mut state = State::default();
        state.microphone.set_preference(preference);
        let line = status(&state);
        assert_eq!(line["microphone"], Value::from(preference.word()));
        assert_eq!(line["mic"], Value::from(mic), "{preference:?}");
    }
}

#[test]
fn the_blocked_microphone_word_lands_on_its_key() {
    let mut state = State::default();
    state.microphone.show(MicCode::Blocked, 0, false);
    let line = status(&state);
    assert_eq!(line["mic"], "blocked");
}
