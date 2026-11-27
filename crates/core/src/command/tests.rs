//! Each command with no link and the counters it raises.

use pods_proto::noise::NoiseMode;
use proptest::prelude::*;
use serde_json::Value;

use super::*;
use crate::legacy::{LegacyRequest, parse_verb};
use crate::state::Microphone;
use crate::status::to_status_line;

pub(crate) fn level(percent: u8) -> AdaptiveLevel {
    AdaptiveLevel::new(percent).unwrap()
}

pub(crate) fn any_command() -> impl Strategy<Value = Command> {
    let mode = prop_oneof![
        Just(NoiseMode::Off),
        Just(NoiseMode::Anc),
        Just(NoiseMode::Transparency),
        Just(NoiseMode::Adaptive),
    ];
    let behavior = prop_oneof![
        Just(EarDetectionBehavior::PauseWhenOneRemoved),
        Just(EarDetectionBehavior::PauseWhenBothRemoved),
        Just(EarDetectionBehavior::Disabled),
    ];
    prop_oneof![
        mode.prop_map(|mode| Command::SetNoise(NoiseTarget::Mode(mode))),
        Just(Command::SetNoise(NoiseTarget::Cycle)),
        behavior.prop_map(Command::SetEarDetection),
        any::<bool>().prop_map(Command::SetConversationalAwareness),
        any::<bool>().prop_map(Command::SetOneBudAnc),
        (0..=ADAPTIVE_LEVEL_MAX).prop_map(|percent| Command::SetAdaptiveLevel(level(percent))),
        any::<bool>().prop_map(Command::SetNotifications),
        proptest::sample::select(MicPref::ALL.to_vec()).prop_map(Command::SetMicrophone),
        Just(Command::Connect),
        Just(Command::Disconnect),
        Just(Command::Forget),
        Just(Command::Reopen),
    ]
}

/// Disconnected states, varied in every field a command reads.
fn any_state() -> impl Strategy<Value = State> {
    let mode = prop_oneof![
        Just(NoiseMode::Off),
        Just(NoiseMode::Anc),
        Just(NoiseMode::Transparency),
        Just(NoiseMode::Adaptive),
    ];
    let behavior = prop_oneof![
        Just(EarDetectionBehavior::PauseWhenOneRemoved),
        Just(EarDetectionBehavior::PauseWhenBothRemoved),
        Just(EarDetectionBehavior::Disabled),
    ];
    (
        mode,
        behavior,
        any::<bool>(),
        any::<bool>(),
        0..=ADAPTIVE_LEVEL_MAX,
    )
        .prop_map(
            |(noise_mode, ear_detection_behavior, ca, one_bud, percent)| State {
                noise_mode,
                ear_detection_behavior,
                conversational_awareness: ca,
                one_bud_anc_mode: one_bud,
                adaptive_noise_level: level(percent),
                ..State::default()
            },
        )
}

proptest! {
    #[test]
    fn with_no_link_only_counters_and_the_values_the_cpp_stores_change(
        state in any_state(),
        command in any_command(),
    ) {
        let mut after = state.clone();
        apply(&mut after, &command);
        let mut expected = state.clone();
        expected.counters = after.counters;
        match command {
            Command::SetEarDetection(behavior) => expected.ear_detection_behavior = behavior,
            Command::SetNotifications(enabled) => expected.notifications = enabled,
            Command::SetMicrophone(preference) => {
                expected.microphone = Microphone::new(preference);
            }
            Command::SetConversationalAwareness(enabled) => {
                expected.conversational_awareness = enabled;
            }
            Command::SetAdaptiveLevel(level) if state.noise_mode == NoiseMode::Adaptive => {
                expected.adaptive_noise_level = level;
            }
            Command::SetNoise(_)
            | Command::SetAdaptiveLevel(_)
            | Command::SetOneBudAnc(_)
            | Command::Connect
            | Command::Disconnect
            | Command::Forget
            | Command::Reopen => {}
        }
        prop_assert_eq!(after, expected);
    }

    #[test]
    fn with_no_link_every_pod_command_is_refused_as_not_connected(
        state in any_state(),
        command in any_command(),
    ) {
        let outcome = apply(&mut state.clone(), &command);
        let expected = match command {
            Command::SetEarDetection(_)
            | Command::SetNotifications(_)
            | Command::SetMicrophone(_) => Outcome::Confirmed,
            Command::Reopen => Outcome::Refused(CommandError::Headless),
            Command::SetNoise(_)
            | Command::SetConversationalAwareness(_)
            | Command::SetOneBudAnc(_)
            | Command::SetAdaptiveLevel(_)
            | Command::Connect
            | Command::Disconnect
            | Command::Forget => Outcome::Refused(CommandError::NotConnected),
        };
        prop_assert_eq!(outcome, expected);
    }
}

#[test]
fn ear_detection_is_stored_and_confirmed() {
    let mut state = State::default();
    let outcome = apply(
        &mut state,
        &Command::SetEarDetection(EarDetectionBehavior::Disabled),
    );
    assert_eq!(outcome, Outcome::Confirmed);
    assert_eq!(state.ear_detection_behavior, EarDetectionBehavior::Disabled);
    assert_eq!(state.counters.ear_detection_changes, 1);
}

#[test]
fn ca_and_adaptive_level_are_stored_without_a_link() {
    let mut state = State::default();
    let outcome = apply(&mut state, &Command::SetConversationalAwareness(true));
    assert_eq!(outcome, Outcome::Refused(CommandError::NotConnected));
    assert!(state.conversational_awareness);

    let mut state = State {
        noise_mode: NoiseMode::Adaptive,
        adaptive_noise_level: level(0),
        ..State::default()
    };
    let outcome = apply(
        &mut state,
        &Command::SetAdaptiveLevel(level(ADAPTIVE_LEVEL_MAX)),
    );
    assert_eq!(outcome, Outcome::Refused(CommandError::NotConnected));
    assert_eq!(state.adaptive_noise_level, level(ADAPTIVE_LEVEL_MAX));

    let mut state = State {
        noise_mode: NoiseMode::Anc,
        adaptive_noise_level: level(0),
        ..State::default()
    };
    apply(&mut state, &Command::SetAdaptiveLevel(level(37)));
    assert_eq!(state.adaptive_noise_level, level(0));
}

/// A variant left out of `ALL` has no index to name here, so it does not build.
fn entry_in_all(error: CommandError) -> CommandError {
    match error {
        CommandError::NotConnected => CommandError::ALL[0],
        CommandError::NotSupported => CommandError::ALL[1],
        CommandError::NotWorn => CommandError::ALL[2],
        CommandError::OutOfRange => CommandError::ALL[3],
        CommandError::BadValue => CommandError::ALL[4],
        CommandError::UnknownKey => CommandError::ALL[5],
        CommandError::BadRequest => CommandError::ALL[6],
        CommandError::Timeout => CommandError::ALL[7],
        CommandError::Headless => CommandError::ALL[8],
        CommandError::NoDevice => CommandError::ALL[9],
    }
}

#[test]
fn all_lists_every_error_once() {
    for (index, error) in CommandError::ALL.into_iter().enumerate() {
        assert_eq!(entry_in_all(error), error);
        assert!(!CommandError::ALL[..index].contains(&error), "{error:?}");
    }
}

#[test]
fn both_documents_name_the_word_of_each_taken_outcome() {
    for document in [
        include_str!("../../../../docs/protocol-1.md"),
        include_str!("../../../../docs/dbus-api.md"),
    ] {
        for outcome in [Outcome::Sent, Outcome::Confirmed] {
            let word = format!("`{}`", outcome.name().unwrap());
            assert!(document.contains(&word), "{word}");
        }
    }
    let refused = Outcome::Refused(CommandError::Timeout);
    assert_eq!(refused.name(), Err(CommandError::Timeout));
}

#[test]
fn codes_are_the_snake_case_variant_names() {
    for error in CommandError::ALL {
        let mut snake = String::new();
        for character in format!("{error:?}").chars() {
            if character.is_ascii_uppercase() && !snake.is_empty() {
                snake.push('_');
            }
            snake.push(character.to_ascii_lowercase());
        }
        assert_eq!(error.code(), snake);
    }
}

#[test]
fn every_error_has_its_own_sentence() {
    let mut seen = Vec::new();
    for error in CommandError::ALL {
        let message = error.message();
        assert!(message.len() > 1, "{error:?}");
        assert!(message.ends_with('.'), "{error:?}: {message}");
        assert!(
            message.starts_with(char::is_uppercase),
            "{error:?}: {message}"
        );
        assert!(!seen.contains(&message), "{error:?} repeats {message}");
        seen.push(message);
    }
    assert_eq!(
        CommandError::NotConnected.message(),
        "The AirPods are not connected."
    );
}

fn command_of(verb: &str) -> Command {
    match parse_verb(verb.as_bytes()) {
        Some(LegacyRequest::Command(command)) => command,
        other => panic!("{verb:?} parses to {other:?}, not a command"),
    }
}

fn counters_of(state: &State) -> Vec<(String, u64)> {
    let Value::Object(fields) = serde_json::from_str(&to_status_line(state)).unwrap() else {
        panic!("the status line is not an object");
    };
    fields
        .into_iter()
        .filter(|(key, _)| key.ends_with("_total"))
        .map(|(key, value)| (key, value.as_u64().unwrap()))
        .collect()
}

fn rises(verb: &str, state: &State) -> Vec<(String, u64)> {
    let before = counters_of(state);
    let mut after = state.clone();
    apply(&mut after, &command_of(verb));
    counters_of(&after)
        .into_iter()
        .zip(before)
        .filter(|((_, now), (_, was))| now != was)
        .map(|((key, now), (_, was))| (key, now.checked_sub(was).unwrap()))
        .collect()
}

#[test]
fn each_verb_raises_its_status_counter_once() {
    let anc = State {
        noise_mode: NoiseMode::Anc,
        ..State::default()
    };
    let adaptive = State {
        noise_mode: NoiseMode::Adaptive,
        ..State::default()
    };
    let disabled = State {
        ear_detection_behavior: EarDetectionBehavior::Disabled,
        ..State::default()
    };
    let aware = State {
        conversational_awareness: true,
        ..State::default()
    };
    let cases = [
        ("noise:off", "noise_control_changes_total", &anc),
        (
            "noise:anc",
            "noise_control_changes_total",
            &State::default(),
        ),
        (
            "noise:transparency",
            "noise_control_changes_total",
            &State::default(),
        ),
        (
            "noise:adaptive",
            "noise_control_changes_total",
            &State::default(),
        ),
        (
            "noise:cycle",
            "noise_control_changes_total",
            &State::default(),
        ),
        ("ear:off", "ear_detection_changes_total", &State::default()),
        ("ear:one", "ear_detection_changes_total", &disabled),
        ("ear:both", "ear_detection_changes_total", &State::default()),
        ("ca:on", "ca_changes_total", &State::default()),
        ("ca:off", "ca_changes_total", &aware),
        ("adaptive:37", "adaptive_level_changes_total", &adaptive),
        ("reopen", "reopen_calls_total", &State::default()),
    ];
    for (verb, key, state) in cases {
        assert_eq!(rises(verb, state), [(key.to_owned(), 1)], "{verb}");
    }
}

#[test]
fn unchanged_or_unavailable_commands_do_not_raise_counters() {
    for verb in [
        "noise:off",
        "ear:one",
        "ca:off",
        "forget",
        "connect",
        "disconnect",
        "adaptive:37",
    ] {
        assert_eq!(rises(verb, &State::default()), [], "{verb}");
    }
    for verb in ["onebud:on", "onebud:off"] {
        let current = State {
            one_bud_anc_mode: verb == "onebud:off",
            ..State::default()
        };
        assert_eq!(rises(verb, &current), [], "{verb}");
    }
    let at_37 = State {
        noise_mode: NoiseMode::Adaptive,
        adaptive_noise_level: level(37),
        ..State::default()
    };
    assert_eq!(rises("adaptive:37", &at_37), []);
}
