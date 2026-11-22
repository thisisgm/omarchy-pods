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

/// Arbitrary bytes, and lines built from the step names and their fields, which arbitrary bytes almost never are.
fn near_steps() -> impl Strategy<Value = Vec<u8>> {
    let word = prop_oneof![
        Just("write-request".to_owned()),
        Just("write-command".to_owned()),
        Just("read".to_owned()),
        Just("control".to_owned()),
        Just("wait".to_owned()),
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
