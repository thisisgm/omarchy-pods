//! The marks of a connection, read from traces the fixture builds and from made up logs.

use pods_core::policy::audio::sink_prefix;
use pods_proto::addr::BdAddr;
use pods_proto::packets::CONTROL_PSM;
use pods_proto::sdulog::Direction;
use proptest::prelude::*;

use super::connect::{Began, Connection, connection};
use super::journal::line_us;
use super::sinks::first_bluez_sink_us;
use super::*;
use crate::probe::hci::fixture::{
    Carried, NO_RESOURCES, OTHER_APPLE_ADVERT, OTHER_COMPANY_ADVERT, OTHER_PODS_ADDRESS,
    PAGE_TIMEOUT, PODS_ADDRESS, PODS_HANDLE, PROXIMITY_ADVERT, RANDOM_ADVERT_ADDRESS,
    REMOTE_USER_ENDED, RESULT_SUCCESS as OPENED, START_UNIX_MICROS, Trace, two_devices,
    whole_connection,
};

/// A made up, locally administered address that no synthetic trace holds.
const ABSENT_ADDRESS: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
/// The three trials of `two_devices`, ten seconds each from the moment the trace starts.
const TRIALS: &str = "1795176000.000000000 trial 1\n\
    1795176010.000000000 trial 2\n\
    1795176020.000000000 trial 3\n";
/// The line the daemon logs as its link comes up, written out so the test pins the words.
const UP: &str = "link: features to up";
const POPULATED: &str = "link: state populated";
/// BlueZ monitor opcodes 0, 1, 6 and 7 reach the skip, and 2 through 5 reach the readers.
const OPCODES: std::ops::Range<u16> = 0..8;

/// Microseconds since 1970 of a moment `ms` after the synthetic traces start.
fn at_us(ms: u64) -> u64 {
    START_UNIX_MICROS.saturating_add(ms.saturating_mul(1_000))
}

fn pods() -> BdAddr {
    BdAddr::from(PODS_ADDRESS)
}

/// The window of trial `trial` of `TRIALS`, counted from 1.
fn trial(trial: usize) -> Window {
    windows(TRIALS).unwrap().remove(trial.saturating_sub(1))
}

/// A window that holds every moment.
fn always() -> Window {
    Window {
        trial: 1,
        from_us: 0,
        to_us: u64::MAX,
    }
}

fn read(trace: &Trace, device: [u8; 6], window: &Window) -> Option<Connection> {
    connection(&trace.bytes(), BdAddr::from(device), window).unwrap()
}

/// One journal line as `journalctl -o json` writes it, with a monotonic stamp of its own.
fn journal_line(realtime_us: u64, monotonic_us: u64, message: &str) -> String {
    format!(
        "{{\"__REALTIME_TIMESTAMP\":\"{realtime_us}\",\"__MONOTONIC_TIMESTAMP\":\"{monotonic_us}\",\"MESSAGE\":\"{message}\"}}\n"
    )
}

/// The daemon's lines for the first and the third trial; the third never populates.
fn journal() -> String {
    [
        journal_line(at_us(1_320), 5_000_000, UP),
        journal_line(at_us(1_350), 5_030_000, POPULATED),
        journal_line(at_us(22_320), 26_000_000, UP),
    ]
    .concat()
}

/// A speaker and, in the first trial, the other pods' decoy sink before the named pods' sink.
fn sinks() -> String {
    let sink = sink_prefix(pods());
    let other = sink_prefix(BdAddr::from(OTHER_PODS_ADDRESS));
    format!(
        "1795176000.500000000 56\talsa_output.speakers\tPipeWire\ts16le 2ch 48000Hz\tRUNNING\n\
     1795176001.500000000 58\t{other}.1\tPipeWire\ts16le 2ch 48000Hz\tRUNNING\n\
     1795176001.900000000 56\talsa_output.speakers\tPipeWire\ts16le 2ch 48000Hz\tRUNNING\n\
     1795176001.900000000 57\t{sink}.1\tPipeWire\ts16le 2ch 48000Hz\tSUSPENDED\n\
     1795176002.000000000 57\t{sink}.1\tPipeWire\ts16le 2ch 48000Hz\tRUNNING\n"
    )
}

/// What the first trial of `two_devices` holds for the pods.
fn first_trial() -> Connection {
    Connection {
        advert_us: Some(at_us(1_000)),
        acl_us: at_us(1_280),
        began: Began::Pods,
        set_configurations: 1,
        open_us: Some(at_us(1_500)),
        start_us: Some(at_us(1_600)),
        control_dials: vec![(at_us(1_300), OPENED)],
        media_us: Some(at_us(1_700)),
        ended: Some(REMOTE_USER_ENDED),
    }
}

/// A trace whose only link is the pods', which they began, carrying `carried`.
fn pods_link(carried: &Carried) -> Trace {
    let mut trace = Trace::new();
    trace.connection_request(PODS_ADDRESS);
    trace.link_up(PODS_HANDLE, PODS_ADDRESS);
    trace.carry(carried);
    trace
}

/// Every way an address is written: with colons or underscores, in capitals or not, and as the bare digits.
fn spellings(address: [u8; 6]) -> Vec<String> {
    let pairs: Vec<String> = address.iter().map(|byte| format!("{byte:02x}")).collect();
    let lower = [pairs.join(":"), pairs.join("_"), pairs.concat()];
    let upper = lower.clone().map(|text| text.to_uppercase());
    lower.into_iter().chain(upper).collect()
}

#[test]
fn a_connection_the_pods_began_reads_pods() {
    let trace = two_devices();
    assert_eq!(read(&trace, PODS_ADDRESS, &trial(1)), Some(first_trial()));
    let host_began = read(&trace, PODS_ADDRESS, &trial(3)).unwrap();
    assert_eq!(host_began.began, Began::Host);
    assert_eq!(host_began.acl_us, at_us(21_400));
    // An advert of the first trial is not one of the third.
    assert_eq!(host_began.advert_us, None);
}

#[test]
fn the_last_request_before_the_link_says_who_began_it() {
    let mut trace = Trace::new();
    trace.create_connection(PODS_ADDRESS);
    trace.connection_complete(PAGE_TIMEOUT, PODS_HANDLE, PODS_ADDRESS);
    trace.connection_request(PODS_ADDRESS);
    trace.link_up(PODS_HANDLE, PODS_ADDRESS);
    assert_eq!(
        read(&trace, PODS_ADDRESS, &always()).unwrap().began,
        Began::Pods
    );
    let mut reversed = Trace::new();
    reversed.connection_request(PODS_ADDRESS);
    reversed.create_connection(PODS_ADDRESS);
    reversed.link_up(PODS_HANDLE, PODS_ADDRESS);
    assert_eq!(
        read(&reversed, PODS_ADDRESS, &always()).unwrap().began,
        Began::Host
    );
    // A page that timed out is no connection, whoever asked for it.
    let mut failed = Trace::new();
    failed.create_connection(PODS_ADDRESS);
    failed.connection_complete(PAGE_TIMEOUT, PODS_HANDLE, PODS_ADDRESS);
    assert_eq!(read(&failed, PODS_ADDRESS, &always()), None);
    // A command for another device began no link of this one, and a link nobody began is refused.
    let mut unbegun = Trace::new();
    unbegun.create_connection(OTHER_PODS_ADDRESS);
    unbegun.link_up(PODS_HANDLE, PODS_ADDRESS);
    let refused = connection(&unbegun.bytes(), pods(), &trial(1));
    assert!(matches!(refused, Err(MarksError::NoBeginning { trial: 1 })));
}

#[test]
fn two_set_configurations_are_counted_as_two() {
    let twice = Carried {
        set_configurations: vec![100, 400],
        ..Carried::default()
    };
    let read_twice = read(&pods_link(&twice), PODS_ADDRESS, &always()).unwrap();
    assert_eq!(read_twice.set_configurations, 2);
    // The answer to a command is not a second command.
    let once = read(&pods_link(&whole_connection()), PODS_ADDRESS, &always()).unwrap();
    assert_eq!(once.set_configurations, 1);
    let third_trial = read(&two_devices(), PODS_ADDRESS, &trial(3)).unwrap();
    assert_eq!(third_trial.set_configurations, 2);
}

#[test]
fn a_refused_control_dial_keeps_its_result_code() {
    let dials = Carried {
        control_dials: vec![(100, NO_RESOURCES), (900, OPENED)],
        ..Carried::default()
    };
    let read_dials = read(&pods_link(&dials), PODS_ADDRESS, &always()).unwrap();
    assert_eq!(
        read_dials.control_dials,
        [(at_us(100), NO_RESOURCES), (at_us(900), OPENED)]
    );
}

#[test]
fn only_the_hosts_answered_dials_of_the_control_channel_are_listed() {
    let mut trace = pods_link(&Carried::default());
    // The pods open a channel of their own, the host one of another protocol, and a last dial gets no answer.
    trace.open(Direction::FromPods, CONTROL_PSM, (0x0044, 0x0054));
    trace.open(Direction::ToPods, 0x0019, (0x0045, 0x0055));
    let mut unanswered = CONTROL_PSM.to_le_bytes().to_vec();
    unanswered.extend(0x0046_u16.to_le_bytes());
    trace.signal(Direction::ToPods, 0x02, &unanswered);
    let read_dials = read(&trace, PODS_ADDRESS, &always()).unwrap();
    assert_eq!(read_dials.control_dials, []);
}

#[test]
fn a_trace_of_two_devices_marks_only_the_named_one() {
    let trace = two_devices();
    assert_eq!(read(&trace, PODS_ADDRESS, &trial(1)), Some(first_trial()));
    // The other pods' link holds the same channel identifiers and is up all the while.
    let other_dials = [
        (1_160, NO_RESOURCES),
        (1_165, OPENED),
        (2_000, NO_RESOURCES),
    ];
    assert_eq!(
        read(&trace, OTHER_PODS_ADDRESS, &trial(1)),
        Some(Connection {
            advert_us: Some(at_us(1_000)),
            acl_us: at_us(1_150),
            began: Began::Host,
            set_configurations: 3,
            open_us: Some(at_us(1_215)),
            start_us: Some(at_us(1_220)),
            control_dials: other_dials.map(|(ms, result)| (at_us(ms), result)).to_vec(),
            media_us: Some(at_us(1_245)),
            ended: Some(REMOTE_USER_ENDED),
        })
    );
    // The second trial is the other pods' alone.
    assert_eq!(read(&trace, PODS_ADDRESS, &trial(2)), None);
    assert!(read(&trace, OTHER_PODS_ADDRESS, &trial(2)).is_some());
    for window in windows(TRIALS).unwrap() {
        assert_eq!(read(&trace, ABSENT_ADDRESS, &window), None);
    }
}

#[test]
fn a_handle_given_out_again_does_not_reach_the_link_that_ended() {
    let mut trace = pods_link(&whole_connection());
    trace.at_ms(3_000);
    trace.end_link(PODS_HANDLE);
    // The other pods take the handle the pods' link had.
    trace.create_connection(OTHER_PODS_ADDRESS);
    trace.link_up(PODS_HANDLE, OTHER_PODS_ADDRESS);
    trace.carry(&Carried {
        control_dials: vec![(4_000, NO_RESOURCES)],
        set_configurations: vec![4_100],
        ..Carried::default()
    });
    let read_first = read(&trace, PODS_ADDRESS, &always()).unwrap();
    assert_eq!(read_first.control_dials, [(at_us(1_300), OPENED)]);
    assert_eq!(read_first.set_configurations, 1);
    assert_eq!(read_first.ended, Some(REMOTE_USER_ENDED));
}

#[test]
fn two_devices_are_written_in_time_order() {
    let mut previous = 0;
    hci::walk(&two_devices().bytes(), |record| {
        assert!(record.at_ns >= previous, "record timestamp went backwards");
        previous = record.at_ns;
    })
    .unwrap();
}

#[test]
#[should_panic(expected = "trace timestamp went backwards")]
fn at_ms_rejects_a_backwards_stamp() {
    let mut trace = Trace::new();
    trace.at_ms(200);
    trace.connection_request(PODS_ADDRESS);
    trace.at_ms(100);
}

#[test]
fn an_advert_is_read_from_both_reports() {
    for legacy in [false, true] {
        let mut trace = Trace::new();
        trace.at_ms(100);
        trace.advert(PODS_ADDRESS, &OTHER_APPLE_ADVERT);
        trace.at_ms(150);
        trace.advert(RANDOM_ADVERT_ADDRESS, &OTHER_COMPANY_ADVERT);
        trace.at_ms(200);
        if legacy {
            trace.legacy_advert(RANDOM_ADVERT_ADDRESS, &PROXIMITY_ADVERT);
        } else {
            trace.advert(RANDOM_ADVERT_ADDRESS, &PROXIMITY_ADVERT);
        }
        // Only the first proximity pairing advert in the window is the mark.
        trace.at_ms(300);
        trace.advert(RANDOM_ADVERT_ADDRESS, &PROXIMITY_ADVERT);
        trace.at_ms(400);
        trace.connection_request(PODS_ADDRESS);
        trace.link_up(PODS_HANDLE, PODS_ADDRESS);
        let advert_us = read(&trace, PODS_ADDRESS, &always()).unwrap().advert_us;
        assert_eq!(advert_us, Some(at_us(200)), "legacy {legacy}");
    }
}

#[test]
fn an_advert_after_the_dial_is_not_counted() {
    const DIAL_MS: u64 = 200;
    const ACL_UP_MS: u64 = 300;
    const BEFORE_DIAL_MS: u64 = 150;

    for (advert_ms, expected) in [
        (BEFORE_DIAL_MS, Some(at_us(BEFORE_DIAL_MS))),
        (250, None),
        (350, None),
    ] {
        let mut trace = Trace::new();
        let mut times = [DIAL_MS, ACL_UP_MS, advert_ms];
        times.sort_unstable();
        for ms in times {
            trace.at_ms(ms);
            match ms {
                DIAL_MS => trace.create_connection(PODS_ADDRESS),
                ACL_UP_MS => trace.link_up(PODS_HANDLE, PODS_ADDRESS),
                _ => trace.advert(PODS_ADDRESS, &PROXIMITY_ADVERT),
            }
        }
        assert_eq!(
            read(&trace, PODS_ADDRESS, &always()).unwrap().advert_us,
            expected,
            "advert at {advert_ms} ms"
        );
    }
}

#[test]
fn an_absent_mark_is_empty_not_zero() {
    let bare = Connection {
        advert_us: None,
        acl_us: at_us(1_280),
        began: Began::Host,
        set_configurations: 0,
        open_us: None,
        start_us: None,
        control_dials: Vec::new(),
        media_us: None,
        ended: None,
    };
    assert_eq!(
        row(&bare, false, None, None, None),
        "host\t\t0\t\t\t\t\t\t\t\t"
    );
    // A mark at the moment the link came up is a zero, and one a microsecond before it is not.
    let at_once = Connection {
        advert_us: Some(at_us(1_280).saturating_sub(1)),
        open_us: Some(at_us(1_280)),
        ..bare
    };
    assert_eq!(
        row(&at_once, false, Some(at_us(1_280)), None, None),
        "host\t-1\t0\t0\t\t\t\t\t0\t\t"
    );
    // A trial with no connection has no count either.
    let (none, _) = table(&two_devices().bytes(), pods(), TRIALS, &journal(), &sinks()).unwrap();
    assert_eq!(none.lines().nth(2), Some("2\t\t\t\t\t\t\t\t\t\t\t"));
}

#[test]
fn the_journal_joins_on_the_wall_clock() {
    // The line was written in the first trial, and its monotonic stamp reads as a moment of the second.
    let journal = journal_line(at_us(1_350), at_us(11_350), POPULATED);
    assert_eq!(line_us(&journal, POPULATED, &trial(1)), Some(at_us(1_350)));
    assert_eq!(line_us(&journal, POPULATED, &trial(2)), None);
}

#[test]
fn the_journal_gives_the_first_whole_line_of_the_window() {
    let journal = [
        journal_line(at_us(500), 1, "link: state populated, as another line says"),
        "{\"__REALTIME_TIMESTAMP\":\"1795176000600000\",\"MESSAGE\":[108,105,110,107]}\n"
            .to_owned(),
        "not a journal line\n".to_owned(),
        journal_line(at_us(1_350), 2, POPULATED),
        journal_line(at_us(1_400), 3, POPULATED),
        journal_line(at_us(11_000), 4, POPULATED),
    ]
    .concat();
    assert_eq!(line_us(&journal, POPULATED, &trial(1)), Some(at_us(1_350)));
    assert_eq!(line_us(&journal, POPULATED, &trial(2)), Some(at_us(11_000)));
    assert_eq!(line_us(&journal, UP, &trial(1)), None);
    assert_eq!(line_us("", POPULATED, &trial(1)), None);
}

#[test]
fn the_first_bluez_sink_of_a_window_is_the_mark() {
    assert_eq!(
        first_bluez_sink_us(&sinks(), pods(), &trial(1)),
        Some(at_us(1_900))
    );
    assert_eq!(first_bluez_sink_us(&sinks(), pods(), &trial(2)), None);
    // A line with no stamp, or with one `date` would not print, is no mark.
    let sink = sink_prefix(pods());
    let unstamped = format!(
        "57\t{sink}.1\n\
        1795176001.9 57\t{sink}.1\n\
        +795176001.900000000 57\t{sink}.1\n"
    );
    assert_eq!(first_bluez_sink_us(&unstamped, pods(), &always()), None);
}

#[test]
fn a_connection_outside_every_window_is_not_counted() {
    // Both of the pods' connections are over, or under way, before the only trial starts.
    let late = "1795176030.000000000 trial 1\n";
    let window = windows(late).unwrap().remove(0);
    assert_eq!(read(&two_devices(), PODS_ADDRESS, &window), None);
    let (read_table, _) =
        table(&two_devices().bytes(), pods(), late, &journal(), &sinks()).unwrap();
    assert_eq!(read_table.lines().nth(1), Some("1\t\t\t\t\t\t\t\t\t\t\t"));
    // A trial ends where the next starts, so the third trial's connection is not the second's.
    assert_eq!(trial(2).to_us, trial(3).from_us);
    assert_eq!(read(&two_devices(), PODS_ADDRESS, &trial(2)), None);
}

#[test]
fn the_trials_log_is_read_whole_or_refused() {
    let spans = |log: &str| -> Vec<(u32, u64, u64)> {
        let read = windows(log).unwrap();
        let span = |window: &Window| (window.trial, window.from_us, window.to_us);
        read.iter().map(span).collect()
    };
    let (second, third) = (at_us(10_000), at_us(20_000));
    assert_eq!(
        spans(TRIALS),
        [
            (1, at_us(0), second),
            (2, second, third),
            (3, third, u64::MAX)
        ]
    );
    // The sample of the format: nanoseconds are cut to microseconds.
    let sample = spans("1732480000.123456789 trial 3\n");
    assert_eq!(sample, [(3, 1_732_480_000_123_456, u64::MAX)]);
    assert!(matches!(windows(""), Err(MarksError::NoTrial)));
    for bad in [
        "1795176000.000000000 trial\n",
        "1795176000.000000000 trial 1 2\n",
        "1795176000.000000000 trail 1\n",
        "1795176000.000000000 trial +1\n",
        "1795176000.000 trial 1\n",
        "1795176000 trial 1\n",
        "-1.000000000 trial 1\n",
        "99999999999999999999.000000000 trial 1\n",
        "\n",
    ] {
        let refused = windows(&format!("{TRIALS}{bad}"));
        let at_line_4 = matches!(refused, Err(MarksError::TrialsLine { line: 4 }));
        assert!(at_line_4, "{bad:?}");
    }
    let backwards = windows(&format!("{TRIALS}1795176019.999999999 trial 4\n"));
    assert!(matches!(
        backwards,
        Err(MarksError::TrialsOrder { line: 4 })
    ));
}

#[test]
fn no_row_holds_an_address() {
    let trace = two_devices().bytes();
    let (read_table, _) = table(&trace, pods(), TRIALS, &journal(), &sinks()).unwrap();
    for address in [PODS_ADDRESS, OTHER_PODS_ADDRESS] {
        for spelling in spellings(address) {
            assert!(!read_table.contains(&spelling), "{read_table}");
        }
    }
    assert!(!read_table.contains("bluez_output"), "{read_table}");
}

/// A trace of whole records with any opcode and any bytes, so the readers past the record header are reached.
fn any_records() -> impl Strategy<Value = Vec<u8>> {
    let record = (OPCODES, proptest::collection::vec(any::<u8>(), 0..40));
    proptest::collection::vec(record, 0..40).prop_map(|records| {
        let mut trace = Trace::new();
        for (opcode, data) in records {
            trace.record(opcode, &data);
        }
        trace.bytes()
    })
}

/// A journal line with any text in each field the reader takes.
fn any_journal_line() -> impl Strategy<Value = String> {
    (".*", ".*").prop_map(|(stamp, message)| {
        serde_json::json!({ "__REALTIME_TIMESTAMP": stamp, "MESSAGE": message }).to_string()
    })
}

proptest! {
    #[test]
    fn any_bytes_as_a_trace_or_a_journal_never_panic(
        bytes in proptest::collection::vec(any::<u8>(), 0..512),
        records in any_records(),
        lines in proptest::collection::vec(any_journal_line(), 0..8),
        from_us in any::<u64>(),
        to_us in any::<u64>(),
    ) {
        let window = Window { trial: 1, from_us, to_us };
        for trace in [&bytes, &records] {
            for address in [PODS_ADDRESS, [0x00; 6]] {
                let _read = connection(trace, BdAddr::from(address), &window);
                let _read_all = connection(trace, BdAddr::from(address), &always());
            }
        }
        let text = String::from_utf8_lossy(&bytes);
        let journal = lines.join("\n");
        for log in [text.as_ref(), journal.as_str()] {
            let _line = line_us(log, POPULATED, &window);
            let _sink = first_bluez_sink_us(log, pods(), &window);
            let _windows = windows(log);
            let _table = table(&records, pods(), TRIALS, log, log);
        }
    }
}
