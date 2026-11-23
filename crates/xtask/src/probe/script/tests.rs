//! The scripts checked against their written description, and the two parsers against arbitrary input.

mod routes;

use proptest::prelude::*;

use super::*;

/// A run length inside the allowed range, for the two scripts that need one.
const SOME_MINUTES: u32 = 2;
/// The shortest and the longest run `--minutes` takes, each with its length in milliseconds.
const EDGE_RUNS: [(u32, u32); 2] = [(1, 60_000), (90, 5_400_000)];
/// The names `--script` takes, written out so a changed name is seen.
const NAMES: [&str; 11] = [
    "knock",
    "listen",
    "soak",
    "voice",
    "side",
    "trials",
    "early",
    "firstword",
    "att-knock",
    "att-map",
    "both",
];
/// How many of `NAMES`, counted from the first, are scripts of the uplink route.
const UPLINK_SCRIPTS: usize = 8;
/// Past twice the longest run, so about half of the draws up to it are allowed minutes and the rest are refused.
const NEAR_RANGE_MINUTES: u32 = 200;

/// The minutes a script must be built with: some for the timed two, none for the rest.
fn minutes_for(name: ScriptName) -> Option<u32> {
    matches!(name, ScriptName::Listen | ScriptName::Soak).then_some(SOME_MINUTES)
}

/// The scripts of the uplink route, in the order of `ScriptName::ALL`.
fn uplink_names() -> Vec<ScriptName> {
    ScriptName::ALL[..UPLINK_SCRIPTS].to_vec()
}

/// The uplink script `name`, which every test written before the routes means.
fn built(name: ScriptName) -> Script {
    build(name, Route::Uplink, minutes_for(name), true).unwrap()
}

fn offsets(script: &Script, wanted: &Action) -> Vec<u32> {
    let events = script.events.iter();
    events
        .filter(|event| event.action == *wanted)
        .map(|event| event.at_ms)
        .collect()
}

#[test]
fn trials_has_twenty_cycles_with_the_stated_gaps() {
    let script = built(ScriptName::Trials);
    let starts = offsets(&script, &Action::Start);
    let stops = offsets(&script, &Action::Stop);
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
    assert_eq!(offsets(&script, &quiet), [70_000]);
    let first = at(0, Action::Cue("Count aloud until told to stop"));
    assert_eq!(script.events.first(), Some(&first));
}

#[test]
fn firstword_has_three_starts_and_no_cue_at_a_start() {
    let script = built(ScriptName::Firstword);
    let starts = offsets(&script, &Action::Start);
    assert_eq!(starts, [8_000, 23_000, 38_000]);
    assert_eq!(offsets(&script, &Action::Stop), [13_000, 28_000, 43_000]);
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
    for name in uplink_names() {
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
                Action::MarkActive
                | Action::MarkIdle
                | Action::DictationStart
                | Action::DictationStop => panic!("{name} touches the dictation stream"),
                Action::Step { .. } | Action::Hold(_) => {
                    panic!("{name} holds a step of a step file")
                }
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
    for name in uplink_names() {
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
                &at(33_000, Action::SetSide(MicSide::Automatic)),
                &at(77_000, Action::RestoreSide),
            ];
            assert_eq!(changes, expected);
        } else {
            assert!(!script.needs_initial_side, "{name}");
            assert!(changes.is_empty(), "{name}");
        }
    }
}

#[test]
fn voice_and_side_do_what_their_scripts_say_and_when() {
    let tap = Action::Cue("Tap the left bud twice");
    let voice = [
        at(0, Action::Start),
        at(3_000, tap.clone()),
        at(
            8_000,
            Action::Cue("Count one to ten slowly, then say the fox sentence"),
        ),
        at(33_000, Action::Cue("Say the fox sentence quietly")),
        at(41_000, Action::Cue("Say the fox sentence loudly")),
        at(
            49_000,
            Action::Cue("Say sss for three seconds, then shh for three seconds"),
        ),
        at(59_000, Action::Cue("Jingle your keys for five seconds")),
        at(67_000, Action::Cue("Stay silent")),
        at(79_000, tap),
        at(84_000, Action::Stop),
        at(89_000, Action::End),
    ];
    assert_eq!(built(ScriptName::Voice).events, voice);
    let side = [
        at(0, Action::Start),
        at(3_000, Action::SetSide(MicSide::Left)),
        at(
            3_000,
            Action::Cue("Say left left left. Tap the left bud twice, then the right bud twice"),
        ),
        at(18_000, Action::SetSide(MicSide::Right)),
        at(
            18_000,
            Action::Cue("Say right right right. Tap the left bud twice, then the right bud twice"),
        ),
        at(33_000, Action::SetSide(MicSide::Automatic)),
        at(
            33_000,
            Action::Cue("Say automatic. Tap the left bud twice, then the right bud twice"),
        ),
        at(
            48_000,
            Action::Cue("Take the left bud out and hold it. Say one two three"),
        ),
        at(
            58_000,
            Action::Cue("Put it back. Take the right bud out. Say one two three"),
        ),
        at(68_000, Action::Cue("Put it back")),
        at(75_000, Action::Stop),
        at(77_000, Action::RestoreSide),
        at(80_000, Action::End),
    ];
    assert_eq!(built(ScriptName::Side).events, side);
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
    for name in uplink_names() {
        let plain = !matches!(name, ScriptName::Knock | ScriptName::Early);
        assert_eq!(built(name).opening == Opening::Handshake, plain, "{name}");
    }
}

#[test]
fn the_rescue_stops_and_never_starts() {
    let rescue = rescue(Route::Uplink).unwrap();
    assert_eq!(rescue.opening, Opening::Handshake);
    assert!(!rescue.needs_initial_side);
    assert!(!rescue.walks());
    assert_eq!(rescue.events, [at(0, Action::Stop), at(0, Action::End)]);
}

#[test]
fn minutes_are_required_for_listen_and_soak_and_refused_elsewhere() {
    for name in uplink_names() {
        let timed = matches!(name, ScriptName::Listen | ScriptName::Soak);
        let without = build(name, Route::Uplink, None, true);
        let with = build(name, Route::Uplink, Some(SOME_MINUTES), true);
        if timed {
            assert_eq!(without, Err(ScriptError::MinutesRequired { name }));
            assert!(with.is_ok(), "{name}");
        } else {
            assert!(without.is_ok(), "{name}");
            assert_eq!(with, Err(ScriptError::MinutesRefused { name }));
        }
    }
    let listen = build(ScriptName::Listen, Route::Uplink, Some(SOME_MINUTES), true).unwrap();
    assert_eq!(listen.events, [at(120_000, Action::End)]);
    let soak = build(ScriptName::Soak, Route::Uplink, Some(SOME_MINUTES), true).unwrap();
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
            let built = build(name, Route::Uplink, Some(minutes), true);
            assert_eq!(built, refused, "{name} {minutes}");
        }
        for (minutes, run_ms) in EDGE_RUNS {
            let script = build(name, Route::Uplink, Some(minutes), true).unwrap();
            let end_ms = script.events.last().unwrap().at_ms;
            let tail_ms = if name == ScriptName::Soak { 5_000 } else { 0 };
            assert_eq!(end_ms, run_ms + tail_ms, "{name} {minutes}");
        }
    }
    // A fixed script refuses the option whatever its value, so the range is never what it reports.
    let refused = Err(ScriptError::MinutesRefused {
        name: ScriptName::Voice,
    });
    assert_eq!(
        build(ScriptName::Voice, Route::Uplink, Some(0), true),
        refused
    );
}

#[test]
fn every_name_reads_back_and_the_refusal_lists_them_all() {
    let names = ScriptName::ALL.map(ScriptName::as_str);
    assert_eq!(names, NAMES);
    for name in ScriptName::ALL {
        assert_eq!(name.to_string().parse(), Ok(name));
    }
    let listed = ScriptError::UnknownName.to_string();
    let list =
        "knock, listen, soak, voice, side, trials, early, firstword, att-knock, att-map and both";
    assert!(listed.ends_with(list), "{listed}");
    for text in ["", "Voice", "voice ", " voice", "voices", "stop"] {
        assert_eq!(text.parse::<ScriptName>(), Err(ScriptError::UnknownName));
    }
}

#[test]
fn the_uplink_route_builds_what_it_built_before_the_routes() {
    // Offsets and counts only, written out so a change of timing or length on the uplink route is seen here.
    let offsets_of = |name| -> Vec<u32> {
        let events = built(name).events;
        events.iter().map(|event| event.at_ms).collect()
    };
    assert_eq!(offsets_of(ScriptName::Knock), [3_000]);
    assert_eq!(offsets_of(ScriptName::Listen), [120_000]);
    assert_eq!(offsets_of(ScriptName::Soak), [0, 0, 120_000, 125_000]);
    assert_eq!(
        offsets_of(ScriptName::Early),
        [5_000, 8_000, 13_000, 18_000]
    );
    assert_eq!(built(ScriptName::Trials).events.len(), 43);
    assert_eq!(built(ScriptName::Firstword).events.len(), 9);
    assert_eq!(uplink_names().len() + 3, ScriptName::ALL.len());
}

#[test]
fn att_knock_opens_the_attribute_channel_alone_and_holds_it() {
    let knock = build(ScriptName::AttKnock, Route::Dictation, None, true).unwrap();
    assert_eq!(knock.opening, Opening::AttKnock);
    assert!(!knock.needs_initial_side);
    assert_eq!(knock.events, [at(3_000, Action::End)]);
    let name = ScriptName::AttKnock;
    let refused = Err(ScriptError::MinutesRefused { name });
    assert_eq!(
        build(name, Route::Dictation, Some(SOME_MINUTES), true),
        refused
    );
}

#[test]
fn att_map_opens_the_attribute_channel_alone_and_asks_for_the_walk() {
    let name = ScriptName::AttMap;
    let map = build(name, Route::Dictation, None, true).unwrap();
    assert_eq!(map.opening, Opening::AttMap);
    assert!(map.walks());
    assert_eq!(map.events, [at(0, Action::End)]);
    let refused = Err(ScriptError::MinutesRefused { name });
    assert_eq!(
        build(name, Route::Dictation, Some(SOME_MINUTES), true),
        refused
    );
    let knock = build(ScriptName::AttKnock, Route::Dictation, None, true).unwrap();
    assert!(!knock.walks());
}

#[test]
fn route_and_script_pairs_outside_the_table_are_refused() {
    for route in Route::ALL {
        let mut taken = Vec::new();
        for name in ScriptName::ALL {
            let without = build(name, route, None, true);
            let with = build(name, route, Some(SOME_MINUTES), true);
            if without.is_ok() || with.is_ok() {
                taken.push(name.to_string());
                continue;
            }
            // A script the route does not take is refused as that with and without minutes, never as a wrong length.
            let off_route = ScriptError::NotOnRoute { name, route };
            assert_eq!(without.unwrap_err(), off_route, "{name} {route}");
            assert_eq!(with.unwrap_err(), off_route, "{name} {route}");
        }
        let (last, rest) = taken.split_last().unwrap();
        let sentence = if rest.is_empty() {
            last.clone()
        } else {
            format!("{} and {last}", rest.join(", "))
        };
        assert_eq!(route.scripts(), sentence, "{route}");
    }
    let refused = build(ScriptName::AttKnock, Route::Uplink, None, true).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "script att-knock is not a script of the uplink route, whose scripts are knock, listen, soak, voice, side, trials, early and firstword"
    );
    let refused = build(ScriptName::Knock, Route::Dictation, None, true).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "script knock is not a script of the dictation route, whose scripts are soak, voice, firstword, att-knock and att-map"
    );
    let refused = build(ScriptName::Soak, Route::Both, Some(SOME_MINUTES), true).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "script soak is not a script of the both route, whose scripts are both"
    );
}

#[test]
fn every_route_reads_back_and_the_refusal_lists_them_all() {
    assert_eq!(
        Route::ALL.map(Route::as_str),
        ["uplink", "dictation", "both"]
    );
    for route in Route::ALL {
        assert_eq!(route.to_string().parse(), Ok(route));
    }
    let listed = ScriptError::UnknownRoute.to_string();
    assert!(listed.ends_with("uplink, dictation and both"), "{listed}");
    for text in ["", "Uplink", "uplink ", "att", "both "] {
        assert_eq!(text.parse::<Route>(), Err(ScriptError::UnknownRoute));
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
    fn name_parse_accepts_only_the_script_names(text in near_names()) {
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
        name in proptest::sample::select(&ScriptName::ALL[..UPLINK_SCRIPTS]),
        minutes in any_minutes(),
    ) {
        let timed = matches!(name, ScriptName::Listen | ScriptName::Soak);
        match (build(name, Route::Uplink, minutes, true), minutes) {
            (Ok(script), Some(minutes)) => {
                prop_assert!(timed && (1..=90).contains(&minutes));
                prop_assert_eq!(script.events.last().map(|event| &event.action), Some(&Action::End));
            }
            (Ok(_), None) => prop_assert!(!timed),
            (Err(error), Some(minutes)) if timed => {
                prop_assert_eq!(error, ScriptError::MinutesOutOfRange { minutes });
                prop_assert!(!(1..=90).contains(&minutes));
            }
            (Err(error), Some(_)) => prop_assert_eq!(error, ScriptError::MinutesRefused { name }),
            (Err(error), None) => {
                prop_assert!(timed);
                prop_assert_eq!(error, ScriptError::MinutesRequired { name });
            }
        }
    }
}
