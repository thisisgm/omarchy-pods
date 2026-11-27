//! The legacy verb grammar, dispatch and numeric bounds.

use proptest::prelude::*;

use super::*;
use crate::command::ADAPTIVE_LEVEL_MAX;

/// The message the C++ dispatch compares with `verb`.
fn text(verb: Verb) -> String {
    match verb {
        Verb::Whole(whole) => whole.to_owned(),
        Verb::Family(family, word) => format!("{family}:{word}"),
    }
}

fn level(verb: &str) -> Option<u8> {
    match parse_verb(verb.as_bytes()) {
        Some(LegacyRequest::Command(Command::SetAdaptiveLevel(level))) => Some(level.percent()),
        None => None,
        Some(other) => panic!("{verb:?} parsed to {other:?}"),
    }
}

#[test]
fn wrong_prefix_is_not_a_level() {
    assert_eq!(
        parse_verb(b"noise:adaptive"),
        Some(noise(NoiseTarget::Mode(NoiseMode::Adaptive)).1)
    );
}

#[test]
fn empty_payload_is_refused() {
    assert_eq!(level("adaptive:"), None);
}

#[test]
fn non_numeric_payload_is_refused() {
    assert_eq!(level("adaptive:high"), None);
}

#[test]
fn in_bounds_payload_parses() {
    assert_eq!(level("adaptive:37"), Some(37));
}

#[test]
fn zero_boundary_parses() {
    assert_eq!(level("adaptive:0"), Some(0));
}

#[test]
fn max_boundary_parses() {
    assert_eq!(level("adaptive:100"), Some(100));
}

#[test]
fn above_max_is_refused() {
    assert_eq!(level("adaptive:101"), None);
}

#[test]
fn below_min_is_refused() {
    assert_eq!(level("adaptive:-1"), None);
}

#[test]
fn custom_range_family_is_not_a_verb_here() {
    // The C++ case drives a hypothetical `mic:` family; the dispatch has no such verb.
    for verb in ["mic:0", "mic:2", "mic:3"] {
        assert_eq!(parse_verb(verb.as_bytes()), None, "{verb}");
    }
}

#[test]
fn leading_whitespace_is_refused() {
    assert_eq!(level("adaptive: 50"), None);
}

#[test]
fn prefix_without_colon_is_refused() {
    assert_eq!(level("adaptive"), None);
}

// What Qt 6.8.2 `QString::toInt` does with the payload, measured against libQt6Core on this host.

#[test]
fn sign_leading_zeros_and_trailing_ascii_space_are_taken_as_qt_takes_them() {
    for (verb, expected) in [
        ("adaptive:+50", Some(50)),
        ("adaptive:-0", Some(0)),
        ("adaptive:0050", Some(50)),
        ("adaptive:50 ", Some(50)),
        ("adaptive:50\n", Some(50)),
        ("adaptive:50\r", Some(50)),
        ("adaptive:50\t", Some(50)),
        ("adaptive:50\u{b}", Some(50)),
        ("adaptive:50\u{c}", Some(50)),
        ("adaptive:50 \n ", Some(50)),
        ("adaptive:5 0", None),
        ("adaptive:50x", None),
        ("adaptive:0x10", None),
        ("adaptive:1e2", None),
        ("adaptive:+", None),
        ("adaptive:+-5", None),
        ("adaptive:- 5", None),
        ("adaptive:\n", None),
        ("adaptive:50\u{a0}", None),
        ("adaptive:50\u{3000}", None),
        ("adaptive:50\u{85}", None),
        ("adaptive:\u{665}", None),
        ("adaptive:2147483648", None),
        ("adaptive:99999999999", None),
    ] {
        assert_eq!(level(verb), expected, "{verb:?}");
    }
}

#[test]
fn bytes_that_are_not_utf8_are_refused() {
    assert_eq!(parse_verb(b"adaptive:5\xff"), None);
    assert_eq!(parse_verb(b"status\xff"), None);
}

#[test]
fn every_whole_word_verb_dispatches() {
    for (verb, request) in VERBS {
        let verb = text(verb);
        assert_eq!(parse_verb(verb.as_bytes()), Some(request), "{verb}");
    }
}

#[test]
fn verbs_carry_no_newline_or_padding() {
    for verb in [
        "status\n",
        " status",
        "status ",
        "noise:anc\n",
        "STATUS",
        "status\0",
    ] {
        assert_eq!(parse_verb(verb.as_bytes()), None, "{verb:?}");
    }
}

/// A payload of a number written the ways Qt reads one, so the property reaches the range check.
fn written_level() -> impl Strategy<Value = (i64, String)> {
    let sign = prop_oneof![Just(""), Just("+"), Just("-")];
    let zeros = 0..4_usize;
    let tail =
        proptest::collection::vec(proptest::sample::select(QT_TRAILING_SPACE.to_vec()), 0..3);
    (0..200_i64, sign, zeros, tail).prop_map(|(magnitude, sign, zeros, tail)| {
        let value = if sign == "-" {
            magnitude.checked_neg().unwrap()
        } else {
            magnitude
        };
        let text = format!(
            "{sign}{}{magnitude}{}",
            "0".repeat(zeros),
            tail.into_iter().collect::<String>()
        );
        (value, text)
    })
}

proptest! {
    #[test]
    fn any_bytes_parse_without_panic_to_a_table_verb_or_a_bounded_level(
        bytes in proptest::collection::vec(any::<u8>(), 0..64),
    ) {
        match parse_verb(&bytes) {
            None => {}
            Some(LegacyRequest::Command(Command::SetAdaptiveLevel(level))) => {
                prop_assert!(bytes.starts_with(ADAPTIVE_PREFIX.as_bytes()));
                prop_assert!(level.percent() <= ADAPTIVE_LEVEL_MAX);
            }
            Some(request) => {
                prop_assert!(VERBS.iter().any(|&(verb, known)| text(verb).as_bytes() == bytes && known == request));
            }
        }
    }

    #[test]
    fn any_bytes_after_the_adaptive_prefix_give_a_bounded_level_or_nothing(
        tail in proptest::collection::vec(any::<u8>(), 0..32),
    ) {
        let bytes = [ADAPTIVE_PREFIX.as_bytes(), &tail].concat();
        if let Some(found) = level_of(&bytes) {
            prop_assert!(found <= ADAPTIVE_LEVEL_MAX);
        }
    }

    #[test]
    fn a_written_level_is_taken_exactly_when_it_is_in_range((value, text) in written_level()) {
        let expected = u8::try_from(value).ok().filter(|level| *level <= ADAPTIVE_LEVEL_MAX);
        prop_assert_eq!(level(&format!("{ADAPTIVE_PREFIX}{text}")), expected);
    }
}

fn level_of(bytes: &[u8]) -> Option<u8> {
    match parse_verb(bytes)? {
        LegacyRequest::Command(Command::SetAdaptiveLevel(level)) => Some(level.percent()),
        other @ (LegacyRequest::Status | LegacyRequest::Command(_)) => {
            panic!("{bytes:?} parsed to {other:?}")
        }
    }
}
