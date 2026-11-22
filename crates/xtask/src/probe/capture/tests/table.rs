//! The walk of the pods' attribute table against fake pods on the attribute channel alone: the map, what cuts it, and what the pods ask meanwhile.

use pods_proto::att;
use pods_proto::sdulog::Direction;

use super::super::super::dictate::{HANDLE_WALK_MAX_REQUESTS, HandleWalk, declarations_request};
use super::super::*;
use super::at;
use super::att_pods::{DECLARED, FIND_INFORMATION, READ_BY_TYPE, TABLE_END};
use super::pods::{ATT_ONLY, Case};

/// A wait for an answer on the attribute channel that a test expects to run out.
pub(super) const SHORT_WALK_MS: u32 = 50;
/// How late a wait that ran out may end on a shared host, well short of the two seconds a real run waits.
pub(super) const WALK_SLACK_NS: u64 = 500_000_000;
/// An Exchange MTU request for 512 bytes, which a peer sends and then waits on.
pub(super) const MTU_REQUEST: [u8; 3] = [0x02, 0x00, 0x02];
/// A Read request for handle 0x0003, which a peer sends and then waits on.
pub(super) const READ_REQUEST: [u8; 3] = [0x0a, 0x03, 0x00];
/// An indication on handle 0x0021, which a peer sends and then waits on.
pub(super) const INDICATION: [u8; 4] = [0x1d, 0x21, 0x00, 0x01];
/// A notification cut short of its handle, which does not parse.
pub(super) const CUT_NOTIFICATION: [u8; 2] = [0x1b, 0x0d];
/// The handles a page of the fake pods' listing holds.
pub(super) const PAGE: u16 = 3;

/// The requests a walk of the fake pods' whole table sends: one for each page, then the one they answer with the error that ends it.
pub(super) fn walk_requests() -> Vec<Vec<u8>> {
    let pages = TABLE_END / PAGE;
    (0..=pages)
        .map(|page| page.saturating_mul(PAGE).saturating_add(1))
        .map(|start| att::find_information_request(start, 0xffff).to_vec())
        .collect()
}

/// Whether a wait of `SHORT_WALK_MS` ended not before it ran out and not after the slack.
fn ran_out_on_time(waited_ns: u64) -> bool {
    let wait_ns = u64::from(SHORT_WALK_MS) * NS_PER_MS;
    (wait_ns..=wait_ns.saturating_add(WALK_SLACK_NS)).contains(&waited_ns)
}

/// A run of the map, which has the attribute channel alone.
fn map_case() -> Case {
    let mut case = Case::new(Opening::AttMap, &[at(0, Action::End)]);
    case.wired = ATT_ONLY;
    case
}

#[test]
fn att_map_lists_the_table_and_reads_the_declarations_once() {
    let report = map_case().run();

    let mut expected = walk_requests();
    expected.push(declarations_request().to_vec());
    assert_eq!(report.pods.att_got, expected);
    assert_eq!(report.pods.heard, []);
    assert_eq!(report.att_logged(Direction::ToPods), report.pods.att_got);
    assert_eq!(report.att_logged(Direction::FromPods), report.pods.att_sent);
    assert_eq!(report.mark("handles"), "present");
    assert_eq!(report.mark("gate"), "map");
    assert_eq!(report.att_dials, 1);
    let lines: Vec<&str> = report.handles.lines().collect();
    let attributes = usize::from(TABLE_END);
    let rows = attributes + DECLARED.len();
    assert_eq!(lines.len(), rows, "{}", report.handles);
    assert_eq!(lines[1], "attribute\t0x0002\t0x2b2a");
    let declarations = [
        "declaration\t0x0006\t0x04\t0x0007\t0x2b2a",
        "declaration\t0x0009\t0x04\t0x000a\t0x2b2a",
        "declaration\t0x000c\t0x10\t0x000d\t0x2b2a",
        "declaration\t0x000f\t0x10\t0x0010\t0x2b2a",
    ];
    assert_eq!(lines[attributes..], declarations);
    let outcome = report.outcome();
    assert_eq!(outcome.att_pdus_in, 8);
    assert_eq!(outcome.end, EndReason::ScriptDone);
}

#[test]
fn att_map_ends_when_a_request_goes_unanswered() {
    let mut silent = map_case();
    silent.pods.att.table = None;
    silent.timing.handle_walk_ms = SHORT_WALK_MS;
    let report = silent.run();
    // The request that got no answer is still out, so the read of the declarations does not follow it.
    assert_eq!(report.pods.att_got, [HandleWalk::first()]);
    assert_eq!(report.mark("handles"), "no listing");
    assert_eq!(report.outcome().end, EndReason::ScriptDone);
    let waited_ns = report.marks("end")[0].0 - report.marks("sent")[0].0;
    assert!(ran_out_on_time(waited_ns), "ended after {waited_ns} ns");

    let mut undeclared = map_case();
    undeclared.pods.att.declares = false;
    undeclared.timing.handle_walk_ms = SHORT_WALK_MS;
    let report = undeclared.run();
    let att_got = &report.pods.att_got;
    assert_eq!(att_got.last().unwrap()[..], declarations_request());
    assert_eq!(att_got.len(), walk_requests().len() + 1);
    assert_eq!(report.handles.lines().count(), usize::from(TABLE_END));
    let closed = format!("the read of the declarations got no answer in {SHORT_WALK_MS} ms");
    assert_eq!(report.mark("att-close"), closed);
    assert_eq!(report.outcome().end, EndReason::ScriptDone);
    let waited_ns = report.marks("end")[0].0 - report.marks("sent").last().unwrap().0;
    assert!(ran_out_on_time(waited_ns), "ended after {waited_ns} ns");

    // A table that lacks a handle is still a map: the mark says what was found and the run is whole.
    let mut lacking = map_case();
    lacking.pods.att.table = Some(vec![0x0001, 0x0007]);
    let report = lacking.run();
    assert_eq!(report.mark("handles"), "missing 0x000a 0x000d 0x0010");
    assert_eq!(report.outcome().end, EndReason::ScriptDone);

    // Pods that declare none of the four answer the read with an error, which ends the wait; a read that ran out of time closes the channel, as above.
    let mut bare = map_case();
    bare.pods.att.table = Some(vec![0x0001]);
    let report = bare.run();
    let refused = att::error_response(READ_BY_TYPE, 0x0001, att::ATTRIBUTE_NOT_FOUND);
    assert_eq!(report.pods.att_sent.last().unwrap()[..], refused);
    assert_eq!(report.handles, "attribute\t0x0001\t0x2b2a\n");
    assert_eq!(report.texts("att-close"), Vec::<&str>::new());
    assert_eq!(report.outcome().end, EndReason::ScriptDone);
}

#[test]
fn a_map_whose_walk_gets_no_answer_marks_the_close_and_asks_nothing_more() {
    let mut case = map_case();
    case.pods.att.answers = Some(2);
    case.timing.handle_walk_ms = SHORT_WALK_MS;
    let report = case.run();

    assert_eq!(report.end(), Some(EndReason::ScriptDone));
    let got = &report.pods.att_got;
    assert_eq!(got.len(), 3, "{got:02x?}");
    assert!(
        got.iter().all(|pdu| pdu[0] == FIND_INFORMATION),
        "{got:02x?}"
    );
    let closed = format!("a request of the walk got no answer in {SHORT_WALK_MS} ms");
    assert_eq!(report.mark("att-close"), closed);
    assert_eq!(
        report.mark("handles"),
        "missing 0x0007 0x000a 0x000d 0x0010"
    );
    assert_eq!(report.att_dials, 1);
}

#[test]
fn a_map_closes_the_channel_on_a_reply_its_walk_cannot_follow() {
    // The pods send this before they read the first request, so that request is still out when it arrives.
    let mut case = map_case();
    case.pods.att.greeting = vec![CUT_NOTIFICATION.to_vec()];
    let report = case.run();

    assert_eq!(report.pods.att_got, [HandleWalk::first()]);
    assert_eq!(
        report.mark("att-close"),
        "the pods sent a reply the walk could not follow"
    );
    assert_eq!(report.mark("handles"), "no listing");
    assert_eq!(report.end(), Some(EndReason::ScriptDone));
    assert_eq!(report.att_dials, 1);
}

#[test]
fn a_map_reads_a_table_that_takes_every_request_it_may_send_and_one_longer_cuts_it() {
    // The pods list one attribute a reply, so a table takes one request more than it holds.
    let most = u16::try_from(HANDLE_WALK_MAX_REQUESTS).unwrap();
    let mut whole = map_case();
    whole.pods.att.page = 1;
    whole.pods.att.table = Some((1..most).collect());
    let report = whole.run();
    let got = &report.pods.att_got;
    assert_eq!(got.len(), usize::from(most) + 1, "{got:02x?}");
    assert_eq!(got.last().unwrap()[..], declarations_request());
    assert_eq!(report.marks("att-close"), []);
    assert_eq!(report.mark("handles"), "present");

    let mut longer = map_case();
    longer.pods.att.page = 1;
    longer.pods.att.table = Some((1..=most).collect());
    let report = longer.run();
    let got = &report.pods.att_got;
    assert_eq!(got.len(), usize::from(most), "{got:02x?}");
    assert!(
        got.iter().all(|pdu| pdu[0] == FIND_INFORMATION),
        "{got:02x?}"
    );
    assert_eq!(
        report.mark("att-close"),
        "the table had not ended after 128 requests"
    );
    assert_eq!(report.mark("handles"), "present");
    assert_eq!(report.end(), Some(EndReason::ScriptDone));
}

#[test]
fn what_the_pods_ask_during_a_map_is_refused_or_confirmed_and_the_map_goes_on() {
    let mut case = map_case();
    case.pods.att.greeting = vec![
        MTU_REQUEST.to_vec(),
        READ_REQUEST.to_vec(),
        INDICATION.to_vec(),
    ];
    let report = case.run();

    // Each refusal names the opcode it answers, no attribute, and the code for a request this host does not serve; `1e` confirms the indication.
    let answers = [
        vec![0x01, 0x02, 0x00, 0x00, 0x06],
        vec![0x01, 0x0a, 0x00, 0x00, 0x06],
        vec![0x1e],
    ];
    let (answered, asked): (Vec<_>, Vec<_>) = report
        .pods
        .att_got
        .iter()
        .cloned()
        .partition(|pdu| answers.contains(pdu));
    assert_eq!(answered, answers);
    let mut expected = walk_requests();
    expected.push(declarations_request().to_vec());
    assert_eq!(asked, expected);
    assert_eq!(report.att_logged(Direction::ToPods), report.pods.att_got);
    let refused = ["opcode 0x02 refused", "opcode 0x0a refused"];
    assert_eq!(report.texts("att-request"), refused);
    let outcome = report.outcome();
    assert_eq!(outcome.att_requests_refused, 2);
    assert_eq!(outcome.end, EndReason::ScriptDone);
}
