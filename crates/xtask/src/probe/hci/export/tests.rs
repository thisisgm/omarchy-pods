//! What the command writes, rendered from traces the fixture builds.

use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
use pods_proto::sdulog::{Direction, MAX_SDU_LEN, parse_line};

use super::super::fixture::{
    ATT_CIDS, AVDTP_PSM, CONTROL_CIDS, Trace, att_packets, control_packets, counting_bytes, session,
};
use super::super::records;
use super::*;

const HOST: Direction = Direction::ToPods;
const PODS: Direction = Direction::FromPods;
/// The identifiers of a second channel.
const SECOND: (u16, u16) = (0x0048, 0x0058);
/// The L2CAP modes the tests ask for: basic and enhanced retransmission.
const BASIC: u8 = 0x00;
const ENHANCED_RETRANSMISSION: u8 = 0x03;
/// A regular file every Linux host has, owned by uid 0.
const SYSTEM_FILE: &str = "/etc/passwd";
/// The event code of a completed synchronous connection.
const EVENT_SYNCHRONOUS_CONNECTION: u8 = 0x2c;
/// One millisecond, the step of the fixture's clock.
const MS: u64 = 1_000_000;

fn rendered(trace: &Trace) -> Vec<Output> {
    render(records(&trace.bytes()).unwrap()).unwrap()
}

fn names(outputs: &[Output]) -> Vec<&str> {
    outputs.iter().map(|output| output.name.as_str()).collect()
}

fn text<'a>(outputs: &'a [Output], name: &str) -> &'a str {
    let found = outputs.iter().find(|output| output.name == name);
    &found.unwrap_or_else(|| panic!("no {name}")).text
}

/// Every line of a log read back with the parser the analysis uses.
fn log(outputs: &[Output], name: &str) -> Vec<(u64, Direction, Vec<u8>)> {
    let text = text(outputs, name);
    assert!(text.is_empty() || text.ends_with('\n'));
    text.lines()
        .map(|line| parse_line(line).unwrap())
        .map(|record| (record.at_ns, record.direction, record.sdu))
        .collect()
}

fn stamped(
    stamps_ms: &[u64],
    packets: Vec<(Direction, Vec<u8>)>,
) -> Vec<(u64, Direction, Vec<u8>)> {
    assert_eq!(stamps_ms.len(), packets.len());
    stamps_ms
        .iter()
        .zip(packets)
        .map(|(ms, (direction, packet))| (ms.saturating_mul(MS), direction, packet))
        .collect()
}

#[test]
fn the_psm_0x1001_channel_is_exported_in_both_directions() {
    let outputs = rendered(&session());
    let expected = stamped(&[7, 8, 9, 10, 13, 15, 16], control_packets());
    assert_eq!(log(&outputs, "sdu.log"), expected);
    let sent_by_host = expected.iter().filter(|line| line.1 == HOST).count();
    assert_eq!((sent_by_host, expected.len()), (4, 7));
    assert_eq!(
        names(&outputs),
        [
            "sdu.log",
            "att.log",
            "psm.tsv",
            "a2dp.tsv",
            "avdtp.txt",
            "links.txt"
        ]
    );
}

#[test]
fn the_psm_0x001f_channel_goes_to_att_log_and_is_absent_when_never_opened() {
    let outputs = rendered(&session());
    assert_eq!(log(&outputs, "att.log"), stamped(&[21, 22], att_packets()));

    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, CONTROL_CIDS);
    trace.send(PODS, CONTROL_CIDS, &[1]);
    trace.connect(PODS, ATT_PSM, ATT_CIDS, 0x0002);
    trace.send(PODS, ATT_CIDS, &[0x1b, 0x0d, 0x00]);
    assert_eq!(
        names(&rendered(&trace)),
        ["sdu.log", "psm.tsv", "a2dp.tsv", "avdtp.txt", "links.txt"]
    );

    let mut trace = Trace::new();
    trace.open(PODS, ATT_PSM, ATT_CIDS);
    trace.open(HOST, AVDTP_PSM, CONTROL_CIDS);
    trace.send(PODS, CONTROL_CIDS, &[1]);
    let outputs = rendered(&trace);
    assert_eq!(
        names(&outputs),
        ["att.log", "psm.tsv", "a2dp.tsv", "avdtp.txt", "links.txt"]
    );
    assert_eq!(text(&outputs, "att.log"), "");
    assert!(
        text(&outputs, "links.txt")
            .contains("log att.log written=0 refused_empty=0 refused_too_long=0\n")
    );
}

#[test]
fn two_channels_on_one_psm_get_a_log_each() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, CONTROL_CIDS);
    trace.open(HOST, CONTROL_PSM, SECOND);
    trace.open(PODS, ATT_PSM, ATT_CIDS);
    trace.open(HOST, CONTROL_PSM, (0x0049, 0x0059));
    trace.send(PODS, SECOND, &[2]);
    trace.send(PODS, CONTROL_CIDS, &[1]);
    trace.send(HOST, (0x0049, 0x0059), &[3]);
    let outputs = rendered(&trace);
    assert_eq!(
        names(&outputs),
        [
            "sdu.log",
            "sdu-2.log",
            "sdu-3.log",
            "att.log",
            "psm.tsv",
            "a2dp.tsv",
            "avdtp.txt",
            "links.txt"
        ]
    );
    assert_eq!(text(&outputs, "sdu.log"), "9000000 < 01\n");
    assert_eq!(text(&outputs, "sdu-2.log"), "8000000 < 02\n");
    assert_eq!(text(&outputs, "sdu-3.log"), "10000000 > 03\n");
}

#[test]
fn a_packet_the_log_cannot_hold_is_counted_not_written() {
    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, CONTROL_CIDS);
    trace.send(PODS, CONTROL_CIDS, &[]);
    trace.send(PODS, CONTROL_CIDS, &counting_bytes(MAX_SDU_LEN + 1));
    trace.send(HOST, CONTROL_CIDS, &counting_bytes(MAX_SDU_LEN));
    trace.send(HOST, CONTROL_CIDS, &[]);
    trace.send(PODS, CONTROL_CIDS, &[7]);
    let outputs = rendered(&trace);
    let lines = log(&outputs, "sdu.log");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].2, counting_bytes(MAX_SDU_LEN));
    assert_eq!(lines[1], (6 * MS, PODS, vec![7]));
    assert!(
        text(&outputs, "links.txt")
            .contains("log sdu.log written=2 refused_empty=2 refused_too_long=1\n")
    );
}

#[test]
fn stamps_count_from_the_first_record_kept() {
    let mut trace = Trace::new();
    trace.record(12, b"a note");
    trace.at_ms(250);
    trace.open(HOST, CONTROL_PSM, CONTROL_CIDS);
    trace.at_ms(1_750);
    trace.send(PODS, CONTROL_CIDS, &[1]);
    trace.at_ms(100);
    trace.send(PODS, CONTROL_CIDS, &[2]);
    let outputs = rendered(&trace);
    assert_eq!(text(&outputs, "sdu.log"), "1500000000 < 01\n0 < 02\n");
    let zero = "realtime_at_zero_ns 1795176000250000000\n";
    assert!(text(&outputs, "links.txt").starts_with(zero));
    assert!(text(&outputs, "psm.tsv").contains("\n0\thost\t0x1001\t"));
}

#[test]
fn the_request_table_lists_sides_results_and_mtu_options() {
    let outputs = rendered(&session());
    let expected = "at_ns\tside\tpsm\tresult\thandle\thost_mtu\tpods_mtu\thost_mode\tpods_mode\n\
        0\tpods\t0x0013\t0x0000\t0x000c\t-\t-\t-\t-\n\
        3000000\thost\t0x1001\t0x0000\t0x000b\t672\t1024\t-\t-\n\
        17000000\tpods\t0x001f\t0x0000\t0x000b\t672\t1024\t-\t-\n\
        996000000\thost\t0x0019\t0x0000\t0x000b\t-\t-\t-\t-\n\
        1004000000\thost\t0x0019\t0x0000\t0x000b\t-\t-\t-\t-\n";
    assert_eq!(text(&outputs, "psm.tsv"), expected);

    let mut trace = Trace::new();
    trace.connect(PODS, ATT_PSM, ATT_CIDS, 0x0003);
    trace.signal(HOST, 0x02, &[0x1f, 0x00, 0x44, 0x00]);
    let expected = "at_ns\tside\tpsm\tresult\thandle\thost_mtu\tpods_mtu\thost_mode\tpods_mode\n\
        0\tpods\t0x001f\t0x0003\t0x000b\t-\t-\t-\t-\n\
        2000000\thost\t0x001f\t-\t0x000b\t-\t-\t-\t-\n";
    assert_eq!(text(&rendered(&trace), "psm.tsv"), expected);
}

#[test]
fn an_attribute_channel_too_small_for_dictation_is_named() {
    let note = |pods_mtu: Option<u16>, psm: u16| {
        let mut trace = Trace::new();
        trace.open(PODS, psm, ATT_CIDS);
        trace.offer_mtu(HOST, ATT_CIDS, 100);
        if let Some(mtu) = pods_mtu {
            trace.offer_mtu(PODS, ATT_CIDS, mtu);
        }
        let outputs = rendered(&trace);
        let links = text(&outputs, "links.txt");
        let notes: Vec<String> = links
            .lines()
            .filter(|line| line.starts_with("att_pods_mtu"))
            .map(str::to_owned)
            .collect();
        notes
    };
    assert_eq!(note(Some(440), ATT_PSM), ["att_pods_mtu_under_441 440"]);
    assert_eq!(note(Some(48), ATT_PSM), ["att_pods_mtu_under_441 48"]);
    assert_eq!(note(Some(441), ATT_PSM), Vec::<String>::new());
    assert_eq!(note(Some(672), ATT_PSM), Vec::<String>::new());
    assert_eq!(note(None, ATT_PSM), Vec::<String>::new());
    assert_eq!(note(Some(48), CONTROL_PSM), Vec::<String>::new());
}

#[test]
fn the_music_tables_of_the_session() {
    let outputs = rendered(&session());
    let seconds = "second\tmedia_packets\tmedia_bytes\trtp_gaps\tlargest_interval_ns\tavdtp_suspend\tavdtp_start\n\
        1\t1\t112\t0\t0\t0\t1\n\
        2\t5\t560\t1\t20000000\t1\t0\n\
        3\t2\t224\t0\t1100000000\t0\t1\n";
    assert_eq!(text(&outputs, "a2dp.tsv"), seconds);
    let codecs = "capability type=0x00 bytes=ffff0235\n\
        capability type=0x02 bytes=80018c84e200\n\
        capability type=0xff vendor=0x0000004c codec=0x8001 bytes=4c0000000180aabb\n\
        configuration type=0x02 bytes=80018c84e200\n";
    assert_eq!(text(&outputs, "avdtp.txt"), codecs);

    let mut trace = Trace::new();
    trace.open(HOST, CONTROL_PSM, CONTROL_CIDS);
    let outputs = rendered(&trace);
    assert_eq!(
        text(&outputs, "a2dp.tsv"),
        "second\tmedia_packets\tmedia_bytes\trtp_gaps\tlargest_interval_ns\tavdtp_suspend\tavdtp_start\n"
    );
    assert_eq!(text(&outputs, "avdtp.txt"), "configuration none\n");
}

#[test]
fn the_link_summary_of_the_session() {
    let outputs = rendered(&session());
    let expected = "realtime_at_zero_ns 1795176000004000000\n\
        synchronous_connection_events 0\n\
        synchronous_connections_opened 0\n\
        acl_packets 44\n\
        malformed_acl_packets 0\n\
        continuations_without_start 0\n\
        unfinished_pdus 0\n\
        malformed_pdus 0\n\
        malformed_signals 0\n\
        responses_without_request 0\n\
        fixed_channel_pdus 0\n\
        requests_over_limit 0\n\
        unmapped_over_limit 0\n\
        not_basic_mode_pdus 0\n\
        avdtp_fragmented_signals 0\n\
        avdtp_malformed_signals 0\n\
        avdtp_other_channels 0\n\
        media_packets_not_rtp 0\n\
        log sdu.log written=7 refused_empty=0 refused_too_long=0\n\
        log att.log written=2 refused_empty=0 refused_too_long=0\n";
    assert_eq!(text(&outputs, "links.txt"), expected);
}

#[test]
fn synchronous_connections_and_what_could_not_be_read_are_counted() {
    let mut trace = Trace::new();
    trace.event(EVENT_SYNCHRONOUS_CONNECTION, &[0x00; 17]);
    trace.event(EVENT_SYNCHRONOUS_CONNECTION, &[0x1a; 17]);
    trace.event(EVENT_SYNCHRONOUS_CONNECTION, &[0x00; 17]);
    trace.acl(PODS, 0b01, &[1, 2, 3]);
    trace.pdu(HOST, 0x0050, &[1]);
    trace.pdu(HOST, 0x0050, &[2]);
    trace.pdu(PODS, 0x0a40, &[3]);
    trace.pdu(PODS, 0x0004, &[4]);
    let outputs = rendered(&trace);
    let links = text(&outputs, "links.txt");
    for line in [
        "synchronous_connection_events 3\n",
        "synchronous_connections_opened 2\n",
        "acl_packets 5\n",
        "continuations_without_start 1\n",
        "fixed_channel_pdus 1\n",
        "unmapped_channel handle=0x000b cid=0x0050 sent_by=host pdus=2\n",
        "unmapped_channel handle=0x000b cid=0x0a40 sent_by=pods pdus=1\n",
    ] {
        assert!(links.contains(line), "{line} is not in {links}");
    }
    assert_eq!(
        names(&outputs),
        ["psm.tsv", "a2dp.tsv", "avdtp.txt", "links.txt"]
    );
}

#[test]
fn a_channel_not_in_basic_mode_is_listed_counted_and_not_exported() {
    let mut trace = Trace::new();
    trace.open(PODS, ATT_PSM, ATT_CIDS);
    trace.offer_mode(PODS, ATT_CIDS, ENHANCED_RETRANSMISSION);
    trace.open(HOST, CONTROL_PSM, CONTROL_CIDS);
    trace.offer_mode(HOST, CONTROL_CIDS, BASIC);
    trace.open(PODS, ATT_PSM, SECOND);
    for cids in [ATT_CIDS, CONTROL_CIDS, SECOND, ATT_CIDS] {
        trace.send(PODS, cids, &[0x1b, 0x0d, 0x00]);
    }
    let outputs = rendered(&trace);
    assert_eq!(
        names(&outputs),
        [
            "sdu.log",
            "att-2.log",
            "psm.tsv",
            "a2dp.tsv",
            "avdtp.txt",
            "links.txt"
        ]
    );
    assert_eq!(text(&outputs, "sdu.log"), "9000000 < 1b0d00\n");
    assert_eq!(text(&outputs, "att-2.log"), "10000000 < 1b0d00\n");
    let requests = "at_ns\tside\tpsm\tresult\thandle\thost_mtu\tpods_mtu\thost_mode\tpods_mode\n\
        0\tpods\t0x001f\t0x0000\t0x000b\t-\t-\t-\t0x03\n\
        3000000\thost\t0x1001\t0x0000\t0x000b\t-\t-\t0x00\t-\n\
        6000000\tpods\t0x001f\t0x0000\t0x000b\t-\t-\t-\t-\n";
    assert_eq!(text(&outputs, "psm.tsv"), requests);
    let links = text(&outputs, "links.txt");
    let said: Vec<&str> = links
        .lines()
        .filter(|line| line.contains("not_basic") || line.starts_with("log "))
        .collect();
    assert_eq!(
        said,
        [
            "not_basic_mode_pdus 2",
            "channel_not_read handle=0x000b psm=0x001f host_mode=- pods_mode=0x03 pdus=2 reason=l2cap_mode_is_not_basic",
            "log sdu.log written=1 refused_empty=0 refused_too_long=0",
            "log att-2.log written=1 refused_empty=0 refused_too_long=0",
        ]
    );
}

#[test]
fn a_cut_trace_is_named_first_and_its_tables_are_those_of_the_whole_records() {
    let bytes = session().bytes();
    let whole = records(&bytes).unwrap();
    assert_eq!(whole.cut, None);
    assert!(!text(&render(whole).unwrap(), "links.txt").contains("trace_cut_short"));

    let cut_bytes = &bytes[..bytes.len() - 100];
    let read = records(cut_bytes).unwrap();
    let cut = read.cut.unwrap();
    let outputs = render(read).unwrap();
    let before = render(records(&bytes[..cut.offset]).unwrap()).unwrap();
    assert_eq!(names(&outputs), names(&before));
    for (cut_output, whole_output) in outputs.iter().zip(&before) {
        if cut_output.name == "links.txt" {
            let named = format!(
                "trace_cut_short record={} offset={}\n{}",
                cut.record, cut.offset, whole_output.text
            );
            assert_eq!(cut_output.text, named);
        } else {
            assert_eq!(cut_output, whole_output);
        }
    }
    assert!(text(&outputs, "a2dp.tsv").lines().count() > 1);
    assert!(text(&before, "links.txt").starts_with("realtime_at_zero_ns "));
}

#[test]
fn a_trace_of_another_owner_is_refused_with_what_to_do_about_it() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("omapods-captures");
    let refused = export(&root, Path::new(SYSTEM_FILE), "20261120T120000Z", "trace-1");
    let error = refused.unwrap_err().to_string();
    assert_eq!(
        error,
        format!(
            "{SYSTEM_FILE} is owned by uid 0, not by the caller, uid {}; create the file yourself with mode 600 before btmon -w writes into it, or make this one yours with chown and chmod 600",
            rustix::process::geteuid().as_raw()
        )
    );
    assert!(!root.exists());
}

#[test]
fn an_empty_trace_renders_empty_tables() {
    let outputs = rendered(&Trace::new());
    assert_eq!(
        names(&outputs),
        ["psm.tsv", "a2dp.tsv", "avdtp.txt", "links.txt"]
    );
    assert!(text(&outputs, "links.txt").starts_with("realtime_at_zero_ns 0\n"));
}
