//! The scripts of the dictation route and of the route that runs both streams, checked against their written table.

use super::super::*;
use super::{SOME_MINUTES, offsets};

/// Every pair of a route and a script that asks for the dictation stream or opens the attribute channel alone.
const PAIRS: [(Route, ScriptName); 6] = [
    (Route::Dictation, ScriptName::AttKnock),
    (Route::Dictation, ScriptName::AttMap),
    (Route::Dictation, ScriptName::Soak),
    (Route::Dictation, ScriptName::Voice),
    (Route::Dictation, ScriptName::Firstword),
    (Route::Both, ScriptName::Both),
];

/// The script of one pair, with the minutes only `soak` takes.
fn built(route: Route, name: ScriptName, marker: bool) -> Script {
    let minutes = (name == ScriptName::Soak).then_some(SOME_MINUTES);
    build(name, route, minutes, marker).unwrap()
}

#[test]
fn the_dictation_scripts_do_what_their_table_says() {
    let soak = built(Route::Dictation, ScriptName::Soak, true);
    let expected = [
        at(0, Action::MarkActive),
        at(480, Action::DictationStart),
        at(480, Action::Cue("Stay as you are. Nothing to do.")),
        at(120_000, Action::DictationStop),
        at(120_500, Action::MarkIdle),
        at(125_000, Action::End),
    ];
    assert_eq!(soak.events, expected);

    let tap = Action::Cue("Tap the left bud twice");
    let voice = built(Route::Dictation, ScriptName::Voice, true);
    let expected = [
        at(0, Action::MarkActive),
        at(480, Action::DictationStart),
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
        at(84_000, Action::DictationStop),
        at(84_500, Action::MarkIdle),
        at(89_000, Action::End),
    ];
    assert_eq!(voice.events, expected);

    let firstword = built(Route::Dictation, ScriptName::Firstword, true);
    let counting = Action::Cue("Count aloud from one and do not stop until told");
    let expected = [
        at(0, counting),
        at(7_520, Action::MarkActive),
        at(8_000, Action::DictationStart),
        at(13_000, Action::DictationStop),
        at(13_500, Action::MarkIdle),
        at(22_520, Action::MarkActive),
        at(23_000, Action::DictationStart),
        at(28_000, Action::DictationStop),
        at(28_500, Action::MarkIdle),
        at(37_520, Action::MarkActive),
        at(38_000, Action::DictationStart),
        at(43_000, Action::DictationStop),
        at(43_500, Action::MarkIdle),
        at(45_000, Action::Cue("Stop counting")),
        at(48_000, Action::End),
    ];
    assert_eq!(firstword.events, expected);

    let both = built(Route::Both, ScriptName::Both, true);
    let expected = [
        at(0, Action::MarkActive),
        at(480, Action::DictationStart),
        at(5_000, Action::Start),
        at(5_000, Action::Cue("Count aloud to twenty")),
        at(60_000, Action::Stop),
        at(65_000, Action::DictationStop),
        at(65_500, Action::MarkIdle),
        at(70_000, Action::End),
    ];
    assert_eq!(both.events, expected);

    let map = built(Route::Dictation, ScriptName::AttMap, true);
    assert_eq!(map.opening, Opening::AttMap);
    assert_eq!(map.events, [at(0, Action::End)]);
    for (route, name) in PAIRS {
        let script = built(route, name, true);
        let alone = matches!(name, ScriptName::AttKnock | ScriptName::AttMap);
        assert_eq!(script.opening == Opening::Handshake, !alone, "{name}");
        assert!(!script.needs_initial_side, "{name}");
        assert!(!script.unchecked, "{name}");
        // Every script but the knock reads the table: the map to list it, the rest before their start write.
        assert_eq!(script.walks(), name != ScriptName::AttKnock, "{name}");
    }
}

#[test]
fn the_minutes_rule_holds_on_the_dictation_and_both_routes() {
    for (route, name) in PAIRS {
        let without = build(name, route, None, true);
        let with = build(name, route, Some(SOME_MINUTES), true);
        if name == ScriptName::Soak {
            assert_eq!(without, Err(ScriptError::MinutesRequired { name }));
            assert!(with.is_ok());
        } else {
            assert!(without.is_ok(), "{name}");
            assert_eq!(with, Err(ScriptError::MinutesRefused { name }), "{name}");
        }
    }
    for minutes in [0, 91] {
        let refused = Err(ScriptError::MinutesOutOfRange { minutes });
        let built = build(ScriptName::Soak, Route::Dictation, Some(minutes), true);
        assert_eq!(built, refused);
    }
    // The soak of this route stops after the minutes asked, at both ends of the range.
    for (minutes, run_ms) in super::EDGE_RUNS {
        let soak = build(ScriptName::Soak, Route::Dictation, Some(minutes), true).unwrap();
        assert_eq!(
            offsets(&soak, &Action::DictationStop),
            [run_ms],
            "{minutes}"
        );
    }
}

#[test]
fn every_script_that_starts_dictation_also_stops_it() {
    for marker in [true, false] {
        for (route, name) in PAIRS {
            let script = built(route, name, marker);
            let (mut dictating, mut uplink, mut starts) = (false, false, 0);
            let mut last_ms = 0;
            for event in &script.events {
                assert!(event.at_ms >= last_ms, "{name} is out of order");
                last_ms = event.at_ms;
                match event.action {
                    Action::DictationStart => {
                        assert!(!dictating, "{name} starts twice at {}", event.at_ms);
                        dictating = true;
                        starts += 1;
                    }
                    Action::DictationStop => {
                        assert!(dictating, "{name} stops what it did not start");
                        dictating = false;
                    }
                    Action::Start => {
                        assert!(!uplink && route == Route::Both, "{name}");
                        uplink = true;
                    }
                    Action::Stop => {
                        assert!(uplink, "{name} stops an uplink it did not start");
                        uplink = false;
                    }
                    Action::End => {
                        assert!(!dictating, "{name} ends with the stream running");
                        assert!(!uplink, "{name} ends with the uplink running");
                    }
                    Action::SetSide(_) | Action::RestoreSide => {
                        panic!("{name} changes the microphone side")
                    }
                    Action::Cue(_) | Action::MarkActive | Action::MarkIdle => {}
                    Action::Step { .. } | Action::Hold(_) => {
                        panic!("{name} holds a step of a step file")
                    }
                }
            }
            assert_eq!(
                script.events.last().map(|event| &event.action),
                Some(&Action::End)
            );
            let streams = !matches!(name, ScriptName::AttKnock | ScriptName::AttMap);
            assert_eq!(starts > 0, streams, "{name}");
            let cycles = if name == ScriptName::Firstword { 3 } else { 1 };
            assert_eq!(starts, if streams { cycles } else { 0 }, "{name}");
        }
    }
}

#[test]
fn every_active_marker_has_an_idle_marker() {
    for (route, name) in PAIRS {
        let script = built(route, name, true);
        let (mut active, mut dictating) = (false, false);
        for event in &script.events {
            match event.action {
                Action::MarkActive => {
                    assert!(!active, "{name} marks active twice at {}", event.at_ms);
                    active = true;
                }
                Action::MarkIdle => {
                    assert!(active, "{name} marks idle a session it did not mark");
                    assert!(!dictating, "{name} marks idle with the stream running");
                    active = false;
                }
                // The stream runs only inside a session that is marked active.
                Action::DictationStart => {
                    assert!(active, "{name} starts outside a marked session");
                    dictating = true;
                }
                Action::DictationStop => dictating = false,
                Action::End => assert!(!active, "{name} ends with the session marked active"),
                Action::Start
                | Action::Stop
                | Action::Cue(_)
                | Action::SetSide(_)
                | Action::RestoreSide => {}
                Action::Step { .. } | Action::Hold(_) => {
                    panic!("{name} holds a step of a step file")
                }
            }
        }
        let marks = offsets(&script, &Action::MarkActive).len();
        assert_eq!(marks, offsets(&script, &Action::MarkIdle).len(), "{name}");
        assert_eq!(
            marks,
            offsets(&script, &Action::DictationStart).len(),
            "{name}"
        );
    }
}

#[test]
fn the_marker_leads_the_start_by_480_ms() {
    assert_eq!(MARKER_LEAD_MS, 480);
    for (route, name) in PAIRS {
        let script = built(route, name, true);
        let starts = offsets(&script, &Action::DictationStart);
        let leads: Vec<u32> = starts.iter().map(|start| start - 480).collect();
        assert_eq!(offsets(&script, &Action::MarkActive), leads, "{name}");
        // The idle marker follows each stop write by half a second.
        let stops = offsets(&script, &Action::DictationStop);
        let lags: Vec<u32> = stops.iter().map(|stop| stop + 500).collect();
        assert_eq!(offsets(&script, &Action::MarkIdle), lags, "{name}");
    }
}

#[test]
fn no_marker_drops_both_markers_and_nothing_else() {
    let is_marker = |event: &Event| matches!(event.action, Action::MarkActive | Action::MarkIdle);
    for (route, name) in PAIRS {
        let with = built(route, name, true);
        let without = built(route, name, false);
        let mut kept = with.events.clone();
        kept.retain(|event| !is_marker(event));
        assert_eq!(without.events, kept, "{name}");
        assert!(!without.events.iter().any(is_marker), "{name}");
        let same = (with.opening, with.needs_initial_side, with.unchecked);
        let rest = (
            without.opening,
            without.needs_initial_side,
            without.unchecked,
        );
        assert_eq!(same, rest, "{name}");
        assert_eq!(with.walks(), without.walks(), "{name}");
        // The markers are there to drop in every script that streams, so the flag is seen to act.
        assert_eq!(
            with.events != without.events,
            with.walks() && name != ScriptName::AttMap
        );
    }
}

#[test]
fn the_dictation_flags_are_refused_on_the_uplink_route() {
    for name in super::uplink_names() {
        let minutes = super::minutes_for(name);
        let refused = build(name, Route::Uplink, minutes, false);
        assert_eq!(refused, Err(ScriptError::NoMarkerOnUplink), "{name}");
        let script = build(name, Route::Uplink, minutes, true).unwrap();
        let refused = script.allow_unchecked(Route::Uplink);
        assert_eq!(refused, Err(ScriptError::UncheckedOnUplink), "{name}");
    }
    assert_eq!(
        ScriptError::NoMarkerOnUplink.to_string(),
        "--no-marker is refused on the uplink route, which sends no session marker"
    );
    assert_eq!(
        ScriptError::UncheckedOnUplink.to_string(),
        "--unchecked is refused on the uplink route, which checks no handle"
    );
    for (route, name) in PAIRS {
        let script = built(route, name, true);
        let events = script.events.clone();
        let waived = script.allow_unchecked(route).unwrap();
        // The flag changes what follows a walk the pods did not answer, and nothing the script sends.
        assert!(waived.unchecked, "{name}");
        assert_eq!(waived.events, events, "{name}");
    }
}

#[test]
fn the_dictation_rescue_stops_marks_idle_and_never_starts() {
    let rescue = rescue(Route::Dictation).unwrap();
    assert_eq!(rescue.opening, Opening::Handshake);
    let expected = [
        at(0, Action::DictationStop),
        at(0, Action::MarkIdle),
        at(0, Action::End),
    ];
    assert_eq!(rescue.events, expected);
    // It writes to a handle the pods were not asked about, which is what a stop that must not fail costs.
    assert!(!rescue.walks() && !rescue.unchecked);
    let refused = super::super::rescue(Route::Both).unwrap_err();
    assert_eq!(refused, ScriptError::RescueOfBoth);
    assert_eq!(
        refused.to_string(),
        "stop takes one route at a time: run it with --route uplink, then with --route dictation"
    );
}
