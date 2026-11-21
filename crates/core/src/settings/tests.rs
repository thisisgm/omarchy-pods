//! Every key, value and action protocol 1 documents, and the refusals around them.

use pods_proto::noise::NoiseMode;
use proptest::prelude::*;

use super::*;
use crate::command::NoiseTarget;
use crate::command::tests::level;
use crate::legacy::{VERBS, Verb};
use crate::state::EarDetectionBehavior;

const PROTOCOL_DOCUMENT: &str = include_str!("../../../../docs/protocol-1.md");

const DBUS_DOCUMENT: &str = include_str!("../../../../docs/dbus-api.md");

fn noise(mode: NoiseMode) -> Command {
    Command::SetNoise(NoiseTarget::Mode(mode))
}

#[test]
fn every_documented_value_sets_its_command() {
    for (key, value, command) in [
        ("noise", "off", noise(NoiseMode::Off)),
        ("noise", "anc", noise(NoiseMode::Anc)),
        ("noise", "transparency", noise(NoiseMode::Transparency)),
        ("noise", "adaptive", noise(NoiseMode::Adaptive)),
        ("noise", "cycle", Command::SetNoise(NoiseTarget::Cycle)),
        (
            "ear_detection",
            "off",
            Command::SetEarDetection(EarDetectionBehavior::Disabled),
        ),
        (
            "ear_detection",
            "one",
            Command::SetEarDetection(EarDetectionBehavior::PauseWhenOneRemoved),
        ),
        (
            "ear_detection",
            "both",
            Command::SetEarDetection(EarDetectionBehavior::PauseWhenBothRemoved),
        ),
        (
            "conversational_awareness",
            "true",
            Command::SetConversationalAwareness(true),
        ),
        (
            "conversational_awareness",
            "false",
            Command::SetConversationalAwareness(false),
        ),
        ("one_bud_anc", "true", Command::SetOneBudAnc(true)),
        ("one_bud_anc", "false", Command::SetOneBudAnc(false)),
        ("adaptive_level", "0", Command::SetAdaptiveLevel(level(0))),
        ("adaptive_level", "37", Command::SetAdaptiveLevel(level(37))),
        (
            "adaptive_level",
            "100",
            Command::SetAdaptiveLevel(level(100)),
        ),
        (
            "adaptive_level",
            "+37",
            Command::SetAdaptiveLevel(level(37)),
        ),
        ("adaptive_level", "-0", Command::SetAdaptiveLevel(level(0))),
        (
            "adaptive_level",
            "37 \t\n",
            Command::SetAdaptiveLevel(level(37)),
        ),
    ] {
        assert_eq!(setting(key, value), Ok(command), "{key} {value}");
    }
}

#[test]
fn a_key_that_is_not_a_setting_is_unknown_key() {
    for key in [
        "volume", "", "Noise", "noise ", "ear", "ca", "onebud", "adaptive",
    ] {
        assert_eq!(
            setting(key, "off"),
            Err(CommandError::UnknownKey),
            "{key:?}"
        );
    }
}

#[test]
fn a_word_the_key_does_not_take_is_bad_request() {
    for (key, value) in [
        ("noise", "loud"),
        ("noise", "ANC"),
        ("noise", ""),
        ("noise", "anc\n"),
        ("ear_detection", "on"),
        ("ear_detection", "two"),
        ("conversational_awareness", "on"),
        ("conversational_awareness", "1"),
        ("one_bud_anc", "off"),
        ("one_bud_anc", "True"),
        ("adaptive_level", ""),
        ("adaptive_level", "high"),
        ("adaptive_level", " 50"),
        ("adaptive_level", "37x"),
        ("adaptive_level", "++37"),
        ("adaptive_level", "37.0"),
        ("adaptive_level", "1e1"),
    ] {
        assert_eq!(
            setting(key, value),
            Err(CommandError::BadRequest),
            "{key} {value:?}"
        );
    }
}

#[test]
fn an_adaptive_level_outside_0_to_100_is_out_of_range() {
    for value in [
        "101",
        "-1",
        "255",
        "256",
        "2147483647",
        "-2147483648",
        "2147483648",
        "-2147483649",
        "99999999999999999999999",
    ] {
        assert_eq!(
            setting("adaptive_level", value),
            Err(CommandError::OutOfRange),
            "{value}"
        );
    }
}

#[test]
fn actions_are_connect_disconnect_and_forget_only() {
    assert_eq!(action("connect"), Ok(Command::Connect));
    assert_eq!(action("disconnect"), Ok(Command::Disconnect));
    assert_eq!(action("forget"), Ok(Command::Forget));
    for name in [
        "reopen",
        "status",
        "noise:off",
        "ear:one",
        "",
        "Connect",
        "connect ",
    ] {
        assert_eq!(action(name), Err(CommandError::BadRequest), "{name:?}");
    }
}

#[test]
fn the_protocol_document_names_every_key_value_and_action() {
    for (key, family) in KEYS {
        let row_start = format!("| `{key}` | ");
        let row = PROTOCOL_DOCUMENT
            .lines()
            .find(|line| line.starts_with(&row_start))
            .unwrap_or_else(|| panic!("docs/protocol-1.md has no row for `{key}`"));
        let phrases: Vec<String> = match family {
            Family::Words(prefix) => VERBS
                .iter()
                .filter_map(|&(verb, _)| match verb {
                    Verb::Family(at, word) if at == prefix => Some(format!("`{word}`")),
                    Verb::Family(..) | Verb::Whole(_) => None,
                })
                .collect(),
            Family::Switch(_) => SWITCH_WORDS
                .iter()
                .map(|(word, _)| format!("`{word}`"))
                .collect(),
            Family::Level => vec!["from 0 to 100".to_owned()],
        };
        for phrase in phrases {
            assert!(
                row.contains(&phrase),
                "the `{key}` row does not name {phrase}: {row}"
            );
        }
    }
    let actions = PROTOCOL_DOCUMENT
        .lines()
        .find(|line| line.starts_with("Asks for one action"))
        .expect("docs/protocol-1.md has no line naming the actions");
    for name in ["connect", "disconnect", "forget"] {
        assert!(action(name).is_ok(), "{name}");
        assert!(actions.contains(&format!("`{name}`")), "{name}: {actions}");
    }
}

#[test]
fn the_dbus_document_has_one_set_method_for_every_key() {
    for (key, _) in KEYS {
        // Sample input: the row `| `SetNoiseMode` | `s` | `noise` | `off`, `anc` |` counts for `noise`.
        let key_cell = format!(" | `{key}` | ");
        let rows = DBUS_DOCUMENT
            .lines()
            .filter(|line| line.starts_with("| `Set") && line.contains(&key_cell))
            .count();
        assert_eq!(
            rows, 1,
            "docs/dbus-api.md has {rows} Set methods for `{key}`"
        );
    }
}

proptest! {
    #[test]
    fn any_key_and_value_give_a_command_or_a_refusal(key in ".{0,32}", value in ".{0,32}") {
        match setting(&key, &value) {
            Ok(_) | Err(CommandError::BadRequest | CommandError::OutOfRange) => {
                prop_assert!(KEYS.iter().any(|(name, _)| *name == key));
            }
            Err(CommandError::UnknownKey) => {
                prop_assert!(KEYS.iter().all(|(name, _)| *name != key));
            }
            Err(other) => prop_assert!(false, "{key:?} {value:?} gave {other:?}"),
        }
    }

    #[test]
    fn any_value_of_a_real_key_gives_a_command_or_a_refusal(
        key in proptest::sample::select(KEYS.iter().map(|(key, _)| *key).collect::<Vec<_>>()),
        value in ".{0,32}",
    ) {
        let result = setting(key, &value);
        prop_assert!(
            matches!(result, Ok(_) | Err(CommandError::BadRequest | CommandError::OutOfRange)),
            "{key} {value:?} gave {result:?}"
        );
    }

    #[test]
    fn any_action_name_is_an_action_or_bad_request(name in ".{0,32}") {
        let result = action(&name);
        prop_assert!(
            matches!(result, Ok(Command::Connect | Command::Disconnect | Command::Forget) | Err(CommandError::BadRequest)),
            "{name:?} gave {result:?}"
        );
    }
}

#[test]
fn every_mode_and_behavior_has_the_word_that_sets_it() {
    for mode in [
        NoiseMode::Off,
        NoiseMode::Anc,
        NoiseMode::Transparency,
        NoiseMode::Adaptive,
    ] {
        let shown = NoiseTarget::Mode(mode).word();
        assert_eq!(setting(NOISE, shown), Ok(noise(mode)), "{mode:?}");
    }
    for behavior in [
        EarDetectionBehavior::PauseWhenOneRemoved,
        EarDetectionBehavior::PauseWhenBothRemoved,
        EarDetectionBehavior::Disabled,
    ] {
        let command = Command::SetEarDetection(behavior);
        assert_eq!(
            setting(EAR_DETECTION, behavior.word()),
            Ok(command),
            "{behavior:?}"
        );
    }
}
