//! A walk the pods did not end, and the tail of a stream that did not stop: what the loop does with a channel it can no longer ask on.

use pods_proto::dictation::{START, STOP};
use pods_proto::sdulog::Direction;
use proptest::prelude::*;

use super::super::super::dictate::{HandleCheck, HandleWalk};
use super::super::guard::TAIL_MAX_PACKETS;
use super::super::*;
use super::att_pods::{FIND_INFORMATION, TABLE_END};
use super::case::{Case, PATIENT_NS, Report};
use super::dictation::{HANDSHAKE_REQUESTS, OPEN_STREAM, STREAM, dictation_case};
use super::table::SHORT_WALK_MS;

/// The opcodes of what the loop asks or writes of its own accord: Find Information, Read By Type and the Write Command of the stream.
const ASKS_AND_WRITES: [u8; 3] = [0x04, 0x08, 0x52];
/// How many runs the property test makes; each takes a walk and a stream on real sockets.
const RUNS: u32 = 24;
/// A reply that lists the four handles and then handle 0x0000, which is behind every request: `05 01`, then `07 00 03 28` for handle 0x0007.
const BACKWARD_LISTING: [u8; 22] = [
    0x05, 0x01, 0x07, 0x00, 0x03, 0x28, 0x0a, 0x00, 0x03, 0x28, 0x0d, 0x00, 0x03, 0x28, 0x10, 0x00,
    0x03, 0x28, 0x00, 0x00, 0x03, 0x28,
];

/// A dictation run whose pods answer `answers` requests of the walk and then no more, with a fresh attribute channel there to be asked for.
fn cut_case(table: Vec<u16>, answers: u32, unchecked: bool) -> Case {
    let mut case = dictation_case(&STREAM);
    case.script.unchecked = unchecked;
    case.pods.att.table = Some(table);
    case.pods.att.answers = Some(answers);
    case.timing.handle_walk_ms = SHORT_WALK_MS;
    case.fresh_att = true;
    case
}

fn by_channel(report: &Report) -> (&[Vec<u8>], &[Vec<u8>]) {
    let got = &report.pods.att_got;
    got.split_at(report.pods.att_fresh_from.unwrap_or(got.len()))
}

#[test]
fn a_walk_cut_with_the_four_handles_listed_or_one_missing_ends_the_run_whatever_was_waived() {
    let whole: Vec<u16> = (1..=TABLE_END).collect();
    let lacking: Vec<u16> = whole
        .iter()
        .copied()
        .filter(|handle| *handle != 0x000a)
        .collect();
    let missing = EndReason::HandleCheck(HandleCheck::Missing(vec![0x000a]));
    let tables = [
        (whole, EndReason::WalkUnfinished, "present"),
        (lacking, missing, "missing 0x000a"),
    ];
    // Six answers of three attributes each list every handle up to 0x0012, so the seventh request is the one left open.
    for (table, end, handles) in &tables {
        for unchecked in [false, true] {
            let name = format!("{handles}, unchecked {unchecked}");
            let report = cut_case(table.clone(), 6, unchecked).run();
            assert_eq!(report.end().as_ref(), Some(end), "{name}");
            assert_eq!(report.mark("handles"), *handles, "{name}");
            let closed = format!("a request of the walk got no answer in {SHORT_WALK_MS} ms");
            assert_eq!(report.mark("att-close"), closed, "{name}");
            assert_eq!(report.att_dials, 1, "{name}");
            let (first, fresh) = by_channel(&report);
            assert_eq!(first.len(), 7, "{first:02x?}");
            assert!(
                first.iter().all(|pdu| pdu[0] == FIND_INFORMATION),
                "{first:02x?}"
            );
            assert_eq!(fresh, Vec::<Vec<u8>>::new(), "{name}");
            assert_eq!(report.pods.got(), HANDSHAKE_REQUESTS, "{name}");
        }
    }
    let end = EndReason::WalkUnfinished.to_string();
    assert_eq!(
        end,
        "the walk of the pods' attribute table was cut before its end, so the start write was not sent"
    );
}

#[test]
fn a_listing_the_walk_cannot_follow_ends_the_run_before_the_start_write() {
    // The pods send this as the channel opens, so the first request is still out when it arrives.
    for unchecked in [false, true] {
        let mut case = dictation_case(&STREAM);
        case.script.unchecked = unchecked;
        case.pods.att.greeting = vec![BACKWARD_LISTING.to_vec()];
        case.fresh_att = true;
        let report = case.run();

        assert_eq!(report.end(), Some(EndReason::WalkUnfinished));
        assert_eq!(report.mark("handles"), "present");
        let closed = "the pods sent a reply the walk could not follow";
        assert_eq!(report.mark("att-close"), closed);
        assert_eq!(report.att_dials, 1);
        assert_eq!(report.pods.att_got, [HandleWalk::first()]);
    }
}

#[test]
fn a_stream_that_ignores_its_stop_ends_the_tail_at_its_packet_bound() {
    let mut case = dictation_case(&OPEN_STREAM);
    case.pods.att.interrupt_after = Some(3);
    case.pods.att.deaf_to_stop = true;
    // Only the packet bound can end this tail before the test's deadline.
    case.timing.stop_tail = PATIENT_NS;
    let report = case.run();

    assert_eq!(report.end(), Some(EndReason::Interrupted));
    assert!(report.returned_ns < PATIENT_NS, "{}", report.returned_ns);
    let stop_ns = report.marks("dictation-stop")[0].0;
    let records = report.att_records();
    let in_the_tail = records
        .iter()
        .filter(|record| record.direction == Direction::FromPods && record.at_ns > stop_ns);
    assert_eq!(
        in_the_tail.count(),
        usize::try_from(TAIL_MAX_PACKETS).unwrap()
    );
    assert_eq!(report.kinds().last(), Some(&"end"));
}

fn any_table() -> impl Strategy<Value = Vec<u16>> {
    prop_oneof![
        Just((1..=TABLE_END).collect::<Vec<u16>>()),
        (1..=TABLE_END).prop_map(|gone| (1..=TABLE_END).filter(|handle| *handle != gone).collect()),
        Just(Vec::new()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(RUNS))]

    #[test]
    fn nothing_is_asked_or_written_on_a_channel_after_a_request_that_got_no_answer(
        table in any_table(),
        answers in 0_u32..9,
        page in 1_usize..5,
        unchecked in any::<bool>(),
    ) {
        let mut case = cut_case(table, answers, unchecked);
        case.pods.att.page = page;
        let report = case.run();
        let (first, fresh) = by_channel(&report);

        // The pods answered the first `answers` requests of the walk, so any after those is one they left open.
        let mut requests = 0_u32;
        let mut left_open = false;
        for pdu in first {
            let asked = ASKS_AND_WRITES.contains(&pdu[0]);
            prop_assert!(!(left_open && asked), "{first:02x?}");
            if pdu[0] == FIND_INFORMATION {
                requests = requests.saturating_add(1);
                left_open = requests > answers;
            }
        }
        // A fresh channel is asked for only after such a request and a walk that listed nothing, carries no request, and only what the owner waived.
        prop_assert_eq!(report.att_dials > 1, left_open && !fresh.is_empty(), "{:?}", report.marks);
        prop_assert!(fresh.is_empty() || report.handles.is_empty(), "{}", report.handles);
        prop_assert!(fresh.is_empty() || unchecked, "{fresh:02x?}");
        prop_assert!(fresh.iter().all(|pdu| *pdu == START || *pdu == STOP), "{fresh:02x?}");
        let outcome = report.outcome();
        prop_assert_eq!(outcome.dictation_starts, outcome.dictation_stops);
    }
}
