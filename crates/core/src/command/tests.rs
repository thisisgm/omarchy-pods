//! What each command does with no link, and the counters it raises, read against the C++ setters.

use cpp_source::MAIN;
use pods_proto::noise::NoiseMode;
use proptest::prelude::*;
use serde_json::Value;

use super::*;
use crate::legacy::{LegacyRequest, parse_verb};
use crate::status::to_status_line;

/// Every command a client can send, for the properties that cover them all.
fn any_command() -> impl Strategy<Value = Command> {
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
        any::<u8>().prop_map(Command::SetAdaptiveLevel),
        Just(Command::Connect),
        Just(Command::Disconnect),
        Just(Command::Forget),
        Just(Command::Reopen),
    ]
}

/// States a disconnected daemon can be in, varied in every field a command reads.
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
            |(noise_mode, ear_detection_behavior, ca, one_bud, level)| State {
                noise_mode,
                ear_detection_behavior,
                conversational_awareness: ca,
                one_bud_anc_mode: one_bud,
                adaptive_noise_level: level,
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
            Command::SetConversationalAwareness(enabled) => {
                expected.conversational_awareness = enabled;
            }
            Command::SetAdaptiveLevel(level) if state.noise_mode == NoiseMode::Adaptive => {
                expected.adaptive_noise_level = level.min(ADAPTIVE_LEVEL_MAX);
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
            Command::SetEarDetection(_) => Outcome::Confirmed,
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

/// Asserts that the C++ `function` sends as a statement of its own, so nothing reads whether the packet went out.
fn assert_send_is_unchecked(function: &str) {
    let body = MAIN.body(&format!("void {function}("));
    let sends: Vec<&str> = body
        .lines()
        .filter(|line| line.contains("writePacketToSocket("))
        .collect();
    assert!(!sends.is_empty(), "{function} sends nothing: {body}");
    for send in sends {
        assert!(
            send.trim_start().starts_with("writePacketToSocket("),
            "{function} checks its send: {send}"
        );
    }
}

// The C++ setters store the value after a send whose failure they ignore.
#[test]
fn ca_and_adaptive_level_are_stored_where_the_cpp_stores_them() {
    for (function, store) in [
        (
            "setConversationalAwareness",
            "m_deviceInfo->setConversationalAwareness(enabled);",
        ),
        (
            "setAdaptiveNoiseLevel",
            "m_deviceInfo->setAdaptiveNoiseLevel(level);",
        ),
    ] {
        assert_send_is_unchecked(function);
        let body = MAIN.body(&format!("void {function}("));
        let (before, after) = body
            .split_once(store)
            .unwrap_or_else(|| panic!("{function} does not store through {store}: {body}"));
        assert!(
            before.contains("writePacketToSocket("),
            "{function}: the store comes before the send: {body}"
        );
        assert!(
            after.contains("++m_"),
            "{function}: no counter after the store"
        );
    }

    let mut state = State::default();
    let outcome = apply(&mut state, &Command::SetConversationalAwareness(true));
    assert_eq!(outcome, Outcome::Refused(CommandError::NotConnected));
    assert!(state.conversational_awareness);

    let mut state = State {
        noise_mode: NoiseMode::Adaptive,
        adaptive_noise_level: 0,
        ..State::default()
    };
    let outcome = apply(&mut state, &Command::SetAdaptiveLevel(u8::MAX));
    assert_eq!(outcome, Outcome::Refused(CommandError::NotConnected));
    assert_eq!(state.adaptive_noise_level, ADAPTIVE_LEVEL_MAX);

    let mut state = State {
        noise_mode: NoiseMode::Anc,
        adaptive_noise_level: 0,
        ..State::default()
    };
    apply(&mut state, &Command::SetAdaptiveLevel(37));
    assert_eq!(state.adaptive_noise_level, 0);
}

#[test]
fn a_level_over_the_maximum_counts_as_the_maximum() {
    let mut state = State {
        noise_mode: NoiseMode::Adaptive,
        adaptive_noise_level: ADAPTIVE_LEVEL_MAX,
        ..State::default()
    };
    apply(&mut state, &Command::SetAdaptiveLevel(u8::MAX));
    assert_eq!(state.counters.adaptive_level_changes, 0);
}

#[test]
fn codes_are_the_snake_case_variant_names() {
    for error in [
        CommandError::NotConnected,
        CommandError::NotSupported,
        CommandError::NotWorn,
        CommandError::OutOfRange,
        CommandError::UnknownKey,
        CommandError::BadRequest,
        CommandError::Timeout,
        CommandError::Headless,
    ] {
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

/// The command a legacy verb carries.
fn command_of(verb: &str) -> Command {
    match parse_verb(verb.as_bytes()) {
        Some(LegacyRequest::Command(command)) => command,
        other => panic!("{verb:?} parses to {other:?}, not a command"),
    }
}

/// The C++ handler of `verb` calls `chain[0]`, each link calls the next, and the body of the last is returned.
fn cpp_chain_body(verb: &str, chain: &[&str]) -> &'static str {
    let handler = if verb.ends_with(':') {
        format!("msg.startsWith(\"{verb}\"))")
    } else {
        format!("msg == \"{verb}\")")
    };
    let mut body = MAIN.body(&handler);
    for function in chain {
        assert!(
            body.contains(&format!("{function}(")),
            "the C++ path of {verb:?} does not call {function}: {body}"
        );
        body = MAIN.body(&format!("void {function}("));
    }
    body
}

/// The status key the C++ publishes for the counter member that `body` increments.
fn cpp_counter_key(body: &str) -> &'static str {
    let (_, after) = body
        .split_once("++m_")
        .unwrap_or_else(|| panic!("no counter rises in {body}"));
    let (member, _) = after.split_once(';').unwrap();
    assert_eq!(
        MAIN.body(&format!("int {member}() const")).trim(),
        format!("return m_{member};"),
        "{member} has no plain getter"
    );
    let suffix = format!("\", {member}()");
    let entry = MAIN
        .all_between("status.insert(\"", ");")
        .into_iter()
        .find(|entry| entry.ends_with(&suffix))
        .unwrap_or_else(|| panic!("no status key publishes {member}"));
    entry.strip_suffix(&suffix).unwrap()
}

/// Every `*_total` key of the status line with its value.
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

/// The counters `verb` raises from `state`, as status keys with how much each rose.
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
fn each_verb_raises_the_counter_its_cpp_setter_raises() {
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
    let noise = ["setNoiseControlModeInt", "setNoiseControlMode"].as_slice();
    let cases: [(&str, &[&str], &State); 12] = [
        ("noise:off", noise, &anc),
        ("noise:anc", noise, &State::default()),
        ("noise:transparency", noise, &State::default()),
        ("noise:adaptive", noise, &State::default()),
        (
            "noise:cycle",
            &[
                "cycleNoiseControlMode",
                "setNoiseControlModeInt",
                "setNoiseControlMode",
            ],
            &State::default(),
        ),
        ("ear:off", &["setEarDetectionBehavior"], &State::default()),
        ("ear:one", &["setEarDetectionBehavior"], &disabled),
        ("ear:both", &["setEarDetectionBehavior"], &State::default()),
        ("ca:on", &["setConversationalAwareness"], &State::default()),
        ("ca:off", &["setConversationalAwareness"], &aware),
        ("adaptive:", &["setAdaptiveNoiseLevel"], &adaptive),
        ("reopen", &["incReopenCallsTotal"], &State::default()),
    ];
    for (verb, chain, state) in cases {
        let key = cpp_counter_key(cpp_chain_body(verb, chain));
        let sent = if verb == "adaptive:" {
            "adaptive:37"
        } else {
            verb
        };
        assert_eq!(rises(sent, state), [(key.to_owned(), 1)], "{verb}");
    }
}

/// Asserts that `guard`, then `then` after it, come before the counter increment in `body`.
fn assert_guard_precedes_counter(verb: &str, body: &str, guard: &str, then: &str) {
    let (_, after_guard) = body
        .split_once(guard)
        .unwrap_or_else(|| panic!("{verb}: no {guard:?} in {body}"));
    let (before_then, after_then) = after_guard
        .split_once(then)
        .unwrap_or_else(|| panic!("{verb}: no {then:?} after {guard:?} in {body}"));
    assert!(
        !before_then.contains("++m_"),
        "{verb}: the counter rises before {then:?}"
    );
    assert!(
        after_then.contains("++m_"),
        "{verb}: no counter after {then:?}"
    );
}

#[test]
fn verbs_whose_cpp_counter_needs_a_link_or_a_change_raise_nothing() {
    let noise = ["setNoiseControlModeInt", "setNoiseControlMode"].as_slice();
    let body = cpp_chain_body("noise:off", noise);
    assert_guard_precedes_counter("noise:off", body, "== mode", "return;");
    assert_send_is_unchecked("setNoiseControlMode");
    assert_eq!(rises("noise:off", &State::default()), []);

    let body = cpp_chain_body("ear:one", &["setEarDetectionBehavior"]);
    assert_guard_precedes_counter(
        "ear:one",
        body,
        "behavior == earDetectionBehavior()",
        "return;",
    );
    assert_eq!(rises("ear:one", &State::default()), []);

    let body = cpp_chain_body("ca:off", &["setConversationalAwareness"]);
    assert_guard_precedes_counter(
        "ca:off",
        body,
        "conversationalAwareness() == enabled",
        "return;",
    );
    assert_eq!(rises("ca:off", &State::default()), []);

    for verb in ["onebud:on", "onebud:off"] {
        let body = cpp_chain_body(verb, &["setOneBudANCMode"]);
        let (sent, unsent) = body
            .split_once("else")
            .unwrap_or_else(|| panic!("{verb}: no branch for an unsent packet in {body}"));
        assert_guard_precedes_counter(verb, sent, "if (writePacketToSocket(", "{");
        assert!(
            !unsent.contains("++m_"),
            "{verb}: a counter rises outside the sent branch"
        );
        let current = State {
            one_bud_anc_mode: verb == "onebud:off",
            ..State::default()
        };
        assert_eq!(rises(verb, &current), [], "{verb}");
    }

    for (verb, function) in [
        ("forget", "forgetDevice"),
        ("connect", "connectAirPods"),
        ("disconnect", "disconnectAirPods"),
    ] {
        let body = cpp_chain_body(verb, &[function]);
        assert_guard_precedes_counter(verb, body, "bluetoothAddress().isEmpty()", "return;");
        assert_eq!(rises(verb, &State::default()), [], "{verb}");
    }

    let body = cpp_chain_body("adaptive:", &["setAdaptiveNoiseLevel"]);
    assert_guard_precedes_counter("adaptive:", body, "adaptiveModeActive()", "{");
    assert_eq!(rises("adaptive:37", &State::default()), []);
    let (_, clamped) = body.split_once("qBound(").unwrap();
    assert_guard_precedes_counter("adaptive:", clamped, "adaptiveNoiseLevel() != level", "{");
    let at_37 = State {
        noise_mode: NoiseMode::Adaptive,
        adaptive_noise_level: 37,
        ..State::default()
    };
    assert_eq!(rises("adaptive:37", &at_37), []);
}

#[test]
fn adaptive_maximum_is_the_cpp_bound() {
    let body = MAIN.body("void setAdaptiveNoiseLevel(");
    let (_, after) = body.split_once("qBound(").unwrap();
    let (bounds, _) = after.split_once(')').unwrap();
    assert_eq!(bounds, format!("0, level, {ADAPTIVE_LEVEL_MAX}"));
}
