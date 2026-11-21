//! The scripts checked against their written description, and the two parsers against arbitrary input.

use proptest::prelude::*;

use super::*;

/// A run length inside the allowed range, for the two scripts that need one.
const SOME_MINUTES: u32 = 2;
/// The names `--script` takes, written out so a changed name is seen.
const NAMES: [&str; 8] = [
    "knock",
    "listen",
    "soak",
    "voice",
    "side",
    "trials",
    "early",
    "firstword",
];
/// Twice the longest run and more, so the property test reaches both ends of the allowed range.
const NEAR_RANGE_MINUTES: u32 = 200;

/// The minutes a script must be built with: some for the timed two, none for the rest.
fn minutes_for(name: ScriptName) -> Option<u32> {
    matches!(name, ScriptName::Listen | ScriptName::Soak).then_some(SOME_MINUTES)
}

fn built(name: ScriptName) -> Script {
    build(name, minutes_for(name)).unwrap()
}

/// The offsets at which a script does `wanted`.
fn offsets(script: &Script, wanted: Action) -> Vec<u32> {
    let events = script.events.iter();
    events
        .filter(|event| event.action == wanted)
        .map(|event| event.at_ms)
        .collect()
}

#[test]
fn trials_has_twenty_cycles_with_the_stated_gaps() {
    let script = built(ScriptName::Trials);
    let starts = offsets(&script, Action::Start);
    let stops = offsets(&script, Action::Stop);
    assert_eq!(starts.len(), 20);
    assert_eq!(stops.len(), 20);
    for (start, stop) in starts.iter().zip(&stops) {
        assert_eq!(*stop, start + 5_000);
    }
    assert_eq!(starts[0], 0, "the first start follows the connect");
    let idle: Vec<u32> = starts[1..]
        .iter()
        .zip(&stops)
        .map(|(start, stop_before)| start - stop_before)
        .collect();
    assert_eq!(idle[..15], [10_000; 15]);
    assert_eq!(idle[15..], [60_000; 4]);
    assert_eq!(script.events.last(), Some(&at(495_000, Action::End)));
    let quiet = Action::Cue("Stop counting. Stay quiet.");
    assert_eq!(offsets(&script, quiet), [70_000]);
    let first = at(0, Action::Cue("Count aloud until told to stop"));
    assert_eq!(script.events.first(), Some(&first));
}

#[test]
fn firstword_has_three_starts_and_no_cue_at_a_start() {
    let script = built(ScriptName::Firstword);
    let starts = offsets(&script, Action::Start);
    assert_eq!(starts, [8_000, 23_000, 38_000]);
    assert_eq!(offsets(&script, Action::Stop), [13_000, 28_000, 43_000]);
    let cues: Vec<&Event> = script
        .events
        .iter()
        .filter(|event| matches!(event.action, Action::Cue(_)))
        .collect();
    let counting = Action::Cue("Count aloud from one and do not stop until told");
    assert_eq!(
        cues,
        [&at(0, counting), &at(45_000, Action::Cue("Stop counting"))]
    );
    for cue in cues {
        assert!(!starts.contains(&cue.at_ms), "a cue at {}", cue.at_ms);
    }
    assert_eq!(script.events.last(), Some(&at(48_000, Action::End)));
}

#[test]
fn every_script_that_starts_also_stops() {
    for name in ScriptName::ALL {
        let script = built(name);
        let mut running = script.opening == Opening::EarlyStart;
        let mut starts = u32::from(running);
        let mut ended = false;
        let mut last_ms = 0;
        for event in &script.events {
            assert!(!ended, "{name} goes on after its end");
            assert!(
                event.at_ms >= last_ms,
                "{name} is out of order at {}",
                event.at_ms
            );
            last_ms = event.at_ms;
            match event.action {
                Action::Start => {
                    assert!(!running, "{name} starts twice at {}", event.at_ms);
                    running = true;
                    starts += 1;
                }
                Action::Stop => {
                    assert!(running, "{name} stops what it did not start");
                    running = false;
                }
                Action::End => ended = true,
                Action::Cue(_) | Action::SetSide(_) | Action::RestoreSide => {}
            }
        }
        assert!(ended, "{name} never ends");
        assert!(!running, "{name} ends with the uplink running");
        let streams = !matches!(name, ScriptName::Knock | ScriptName::Listen);
        assert_eq!(starts > 0, streams, "{name}");
    }
}

#[test]
fn only_side_changes_the_microphone_side_and_it_puts_it_back() {
    for name in ScriptName::ALL {
        let script = built(name);
        let changes: Vec<&Event> = script
            .events
            .iter()
            .filter(|event| matches!(event.action, Action::SetSide(_) | Action::RestoreSide))
            .collect();
        if name == ScriptName::Side {
            assert!(script.needs_initial_side);
            let expected = [
                &at(3_000, Action::SetSide(MicSide::Left)),
                &at(18_000, Action::SetSide(MicSide::Right)),
                &at(33_000, Action::RestoreSide),
            ];
            assert_eq!(changes, expected);
        } else {
            assert!(!script.needs_initial_side, "{name}");
            assert!(changes.is_empty(), "{name}");
        }
    }
}

#[test]
fn knock_and_early_open_as_their_names_say() {
    let knock = built(ScriptName::Knock);
    assert_eq!(knock.opening, Opening::Knock);
    assert_eq!(knock.events, [at(3_000, Action::End)]);
    let early = built(ScriptName::Early);
    assert_eq!(early.opening, Opening::EarlyStart);
    let expected = [
        at(5_000, Action::Stop),
        at(8_000, Action::Start),
        at(13_000, Action::Stop),
        at(18_000, Action::End),
    ];
    assert_eq!(early.events, expected);
    for name in ScriptName::ALL {
        let plain = !matches!(name, ScriptName::Knock | ScriptName::Early);
        assert_eq!(built(name).opening == Opening::Handshake, plain, "{name}");
    }
}

#[test]
fn the_rescue_stops_and_never_starts() {
    let rescue = rescue();
    assert_eq!(rescue.opening, Opening::Handshake);
    assert!(!rescue.needs_initial_side);
    assert_eq!(rescue.events, [at(0, Action::Stop), at(0, Action::End)]);
}

#[test]
fn minutes_are_required_for_listen_and_soak_and_refused_elsewhere() {
    for name in ScriptName::ALL {
        let timed = matches!(name, ScriptName::Listen | ScriptName::Soak);
        let without = build(name, None);
        let with = build(name, Some(SOME_MINUTES));
        if timed {
            assert_eq!(without, Err(ScriptError::MinutesRequired { name }));
            assert!(with.is_ok(), "{name}");
        } else {
            assert!(without.is_ok(), "{name}");
            assert_eq!(with, Err(ScriptError::MinutesRefused { name }));
        }
    }
    let listen = build(ScriptName::Listen, Some(SOME_MINUTES)).unwrap();
    assert_eq!(listen.events, [at(120_000, Action::End)]);
    let soak = build(ScriptName::Soak, Some(SOME_MINUTES)).unwrap();
    let expected = [
        at(0, Action::Start),
        at(0, Action::Cue("Stay as you are. Nothing to do.")),
        at(120_000, Action::Stop),
        at(125_000, Action::End),
    ];
    assert_eq!(soak.events, expected);
}

#[test]
fn minutes_outside_1_to_90_are_refused() {
    for name in [ScriptName::Listen, ScriptName::Soak] {
        for minutes in [0, 91, u32::MAX] {
            let refused = Err(ScriptError::MinutesOutOfRange { minutes });
            assert_eq!(build(name, Some(minutes)), refused, "{name} {minutes}");
        }
        for (minutes, run_ms) in [(1, 60_000), (90, 5_400_000)] {
            let script = build(name, Some(minutes)).unwrap();
            let end_ms = script.events.last().unwrap().at_ms;
            let tail_ms = if name == ScriptName::Soak { 5_000 } else { 0 };
            assert_eq!(end_ms, run_ms + tail_ms, "{name} {minutes}");
        }
    }
    // A fixed script refuses the option whatever its value, so the range is never what it reports.
    let refused = Err(ScriptError::MinutesRefused {
        name: ScriptName::Voice,
    });
    assert_eq!(build(ScriptName::Voice, Some(0)), refused);
}

#[test]
fn every_name_reads_back_and_the_refusal_lists_them_all() {
    let names = ScriptName::ALL.map(ScriptName::as_str);
    assert_eq!(names, NAMES);
    for name in ScriptName::ALL {
        assert_eq!(name.to_string().parse(), Ok(name));
    }
    let listed = ScriptError::UnknownName.to_string();
    let list = "knock, listen, soak, voice, side, trials, early and firstword";
    assert!(listed.ends_with(list), "{listed}");
    for text in ["", "Voice", "voice ", " voice", "voices", "stop"] {
        assert_eq!(text.parse::<ScriptName>(), Err(ScriptError::UnknownName));
    }
}

/// Arbitrary text, and text near a name: a name, a name in capitals, and a name with text after it.
fn near_names() -> impl Strategy<Value = String> {
    let name = proptest::sample::select(&NAMES[..]);
    prop_oneof![
        any::<String>(),
        name.clone().prop_map(str::to_owned),
        name.clone().prop_map(str::to_uppercase),
        (name, any::<String>()).prop_map(|(name, tail)| format!("{name}{tail}")),
    ]
}

/// Any minutes at all, and minutes around the allowed range, which arbitrary ones almost never reach.
fn any_minutes() -> impl Strategy<Value = Option<u32>> {
    proptest::option::of(prop_oneof![any::<u32>(), 0_u32..=NEAR_RANGE_MINUTES])
}

proptest! {
    #[test]
    fn name_parse_accepts_only_the_eight_names(text in near_names()) {
        match text.parse::<ScriptName>() {
            Ok(name) => prop_assert_eq!(name.as_str(), text),
            Err(error) => {
                prop_assert_eq!(error, ScriptError::UnknownName);
                prop_assert!(!NAMES.contains(&text.as_str()));
            }
        }
    }

    #[test]
    fn build_obeys_the_minutes_rule_for_any_input(
        name in proptest::sample::select(&ScriptName::ALL[..]),
        minutes in any_minutes(),
    ) {
        let timed = matches!(name, ScriptName::Listen | ScriptName::Soak);
        match (build(name, minutes), minutes) {
            (Ok(script), Some(minutes)) => {
                prop_assert!(timed && (1..=90).contains(&minutes));
                prop_assert_eq!(script.events.last().map(|event| event.action), Some(Action::End));
            }
            (Ok(_), None) => prop_assert!(!timed),
            (Err(error), Some(minutes)) if timed => {
                prop_assert_eq!(error, ScriptError::MinutesOutOfRange { minutes });
                prop_assert!(!(1..=90).contains(&minutes));
            }
            (Err(error), Some(_)) => prop_assert_eq!(error, ScriptError::MinutesRefused { name }),
            (Err(error), None) => prop_assert_eq!(error, ScriptError::MinutesRequired { name }),
        }
    }
}
