//! The step file parser on its edges, its guards and arbitrary input; no user path drives these, the command tests do.

use proptest::prelude::*;

use super::*;

fn att(line: usize, step: AttStep) -> Action {
    let packet = StepPacket::Att(step);
    Action::Step { line, packet }
}

#[test]
fn the_documented_sample_fires_each_step_at_the_sum_of_the_waits_above_it() {
    let text = b"# arm, then start\nwrite-request 0x0010 0100010101\nwait 80\ncontrol 040004004b000300030103\nwrite-command 0x0007 0100\nwait 2000\n";
    let events = parse_steps(text).unwrap();
    let arm = AttStep::WriteRequest {
        handle: 0x0010,
        value: vec![0x01, 0x00, 0x01, 0x01, 0x01],
    };
    let expected = [
        at(0, att(2, arm)),
        at(80, Action::MarkActive),
        at(80, Action::DictationStart),
        at(2_080, Action::End),
    ];
    assert_eq!(events, expected);
}

#[test]
fn each_packet_a_guard_covers_goes_out_under_its_guard() {
    let cases = [
        (
            "control 04000400580000000900000182000000049600",
            Action::Start,
        ),
        ("control 040004005800000002000301", Action::Stop),
        ("control 040004004b000300030103", Action::MarkActive),
        ("control 040004004b000300030000", Action::MarkIdle),
        ("write-command 0x0007 0100", Action::DictationStart),
        ("write-command 0x000a 010000", Action::DictationStop),
    ];
    for (line, action) in cases {
        let events = parse_steps(line.as_bytes()).unwrap();
        assert_eq!(events, [at(0, action), at(0, Action::End)], "{line}");
    }
}

#[test]
fn each_step_kind_leaves_as_its_pdu_and_is_marked_by_its_kind_and_handle() {
    let cases = [
        (
            AttStep::WriteRequest {
                handle: 0x000e,
                value: vec![0x01, 0x00],
            },
            vec![0x12, 0x0e, 0x00, 0x01, 0x00],
            "write-request 0x000e",
        ),
        (
            AttStep::WriteCommand {
                handle: 0x0102,
                value: vec![0x31],
            },
            vec![0x52, 0x02, 0x01, 0x31],
            "write-command 0x0102",
        ),
        (
            AttStep::Read { handle: 0xffff },
            vec![0x0a, 0xff, 0xff],
            "read 0xffff",
        ),
    ];
    for (step, pdu, mark) in cases {
        assert_eq!(step.pdu(), pdu);
        assert_eq!(StepPacket::Att(step).to_string(), mark);
    }
    assert_eq!(StepPacket::Control(vec![0x04]).to_string(), "control");
}

#[test]
fn every_bound_is_reached_and_not_passed() {
    let longest = format!("write-request 0xffff {}", "ab".repeat(MAX_VALUE_BYTES));
    let events = parse_steps(longest.as_bytes()).unwrap();
    let value = vec![0xab; MAX_VALUE_BYTES];
    let step = AttStep::WriteRequest {
        handle: 0xffff,
        value,
    };
    assert_eq!(events[0], at(0, att(1, step)));
    let longest_file = u64::try_from((longest.len() + 1) * MAX_LINES).unwrap();
    assert!(longest_file <= MAX_FILE_BYTES);
    let longest_wait = format!("wait {MAX_WAIT_MS}\n");
    let waits = usize::try_from(MAX_RUN_MS / MAX_WAIT_MS).unwrap();
    let longest_run = longest_wait.repeat(waits);
    assert_eq!(
        parse_steps(longest_run.as_bytes()).unwrap(),
        [at(MAX_RUN_MS, Action::End)]
    );
    let refused = parse_steps((longest_run + "wait 1\n").as_bytes()).unwrap_err();
    let line = waits + 1;
    let fault = Fault::RunTooLong;
    assert_eq!(refused, StepsError { line, fault });
    let most_lines = "read 0x0001\n".repeat(MAX_LINES);
    assert_eq!(
        parse_steps(most_lines.as_bytes()).unwrap().len(),
        MAX_LINES + 1
    );
    let refused = parse_steps((most_lines + "\n").as_bytes()).unwrap_err();
    let (line, fault) = (MAX_LINES + 1, Fault::TooManyLines);
    assert_eq!(refused, StepsError { line, fault });
}

#[test]
fn a_read_of_a_stream_handle_and_a_wait_of_nothing_are_steps_like_any() {
    let events = parse_steps(b"read 0x0007\nwait 0\nread 0x000a").unwrap();
    let read = |line, handle| att(line, AttStep::Read { handle });
    let expected = [
        at(0, read(1, START_HANDLE)),
        at(0, read(3, STOP_HANDLE)),
        at(0, Action::End),
    ];
    assert_eq!(events, expected);
}

fn hold(text: &str) -> Action {
    Action::Hold(text.to_owned())
}

#[test]
fn a_cue_holds_its_text_at_the_sum_of_the_waits_above_it() {
    let events =
        parse_steps(b"wait 20\n  cue \"controller: restore now\"\nwait 5\ncue \"\"\n").unwrap();
    let expected = [
        at(20, hold("controller: restore now")),
        at(25, hold("")),
        at(25, Action::End),
    ];
    assert_eq!(events, expected);
    let longest = "\u{e9}".repeat(MAX_CUE_CHARS);
    let events = parse_steps(format!("cue \"{longest}\"").as_bytes()).unwrap();
    assert_eq!(events, [at(0, hold(&longest)), at(0, Action::End)]);
}

#[test]
fn a_cue_without_quotes_with_an_open_quote_or_past_the_bound_is_no_step() {
    let too_long = format!("cue \"{}\"", "a".repeat(MAX_CUE_CHARS + 1));
    let refused = [
        "cue",
        "cue restore now",
        "cue \"restore now",
        "cue restore now\"",
        "cue \"",
        "cue  \"two spaces\"",
        "cue\t\"a tab\"",
        "cue \"a quote \" inside\"",
        "cue \"a tab\tinside\"",
        "cue \"an escape \u{1b}[2J inside\"",
        "cue \"text\" 5",
        "cue\"no space\"",
        too_long.as_str(),
    ];
    for line in refused {
        let (line_number, fault) = (1, Fault::UnknownStep);
        let expected = StepsError {
            line: line_number,
            fault,
        };
        assert_eq!(parse_steps(line.as_bytes()), Err(expected), "{line}");
    }
}

/// The step file `name` of this crate, as the sitting runs it.
fn step_file(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("steps");
    std::fs::read(path.join(name)).unwrap()
}

#[test]
fn all_four_files_load_and_name_no_unknown_step() {
    let now = || hold("controller: restore now");
    let revive = vec![
        at(0, hold("music playing, both buds in")),
        at(0, Action::Start),
        at(10_000, Action::Stop),
        at(11_500, hold("is one bud silent?")),
        at(16_500, Action::Start),
        at(21_500, hold("did sound come back in both buds?")),
        at(26_500, Action::Stop),
        at(28_000, now()),
        at(28_000, Action::End),
    ];
    let mut restore: Vec<Event> = (0..8_u32)
        .flat_map(|cycle| {
            let from = cycle * 21_500;
            let form = hold(&format!("controller: restore form {}", cycle + 1));
            [
                at(from, Action::Start),
                at(from + 5_000, Action::Stop),
                at(from + 6_500, form),
            ]
        })
        .collect();
    restore.push(at(172_000, Action::End));
    let mut stop_early: Vec<Event> = (0..3_u32)
        .flat_map(|cycle| {
            let from = cycle * 8_000;
            [
                at(from, Action::Start),
                at(from, Action::Stop),
                at(from + 3_000, now()),
            ]
        })
        .collect();
    stop_early.push(at(24_000, Action::End));
    let start_twice = vec![
        at(0, Action::Start),
        at(3_000, Action::Start),
        at(6_000, Action::Stop),
        at(7_500, now()),
        at(7_500, Action::End),
    ];
    let files = [
        ("revive", revive),
        ("restore", restore),
        ("stop-early", stop_early),
        ("start-twice", start_twice),
    ];
    for (name, expected) in files {
        assert_eq!(parse_steps(&step_file(name)), Ok(expected), "{name}");
    }
}

/// Arbitrary bytes, and lines built from the step names and their fields, which arbitrary bytes almost never are.
fn near_steps() -> impl Strategy<Value = Vec<u8>> {
    let word = prop_oneof![
        Just("write-request".to_owned()),
        Just("write-command".to_owned()),
        Just("read".to_owned()),
        Just("control".to_owned()),
        Just("wait".to_owned()),
        Just("cue".to_owned()),
        "\"[a-z \"]{0,8}\"?",
        Just("#".to_owned()),
        "0x[0-9a-fA-F]{0,6}",
        "[0-9a-f]{0,12}",
        "[0-9+-]{0,8}",
    ];
    let line = proptest::collection::vec(word, 0..5).prop_map(|words| words.join(" "));
    let lines = proptest::collection::vec(line, 0..20).prop_map(|lines| lines.join("\n"));
    prop_oneof![
        proptest::collection::vec(any::<u8>(), 0..512),
        lines.prop_map(String::into_bytes),
    ]
}

proptest! {
    #[test]
    fn parse_steps_never_panics_and_ends_every_plan_last(text in near_steps()) {
        match parse_steps(&text) {
            Ok(events) => {
                let last = events.last().map(|event| &event.action);
                prop_assert_eq!(last, Some(&Action::End));
                prop_assert!(events.windows(2).all(|pair| matches!(pair, [a, b] if a.at_ms <= b.at_ms)));
            }
            Err(error) => prop_assert!((1..=MAX_LINES + 1).contains(&error.line)),
        }
    }
}
