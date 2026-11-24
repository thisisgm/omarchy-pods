//! The built binary's `probe hci` command on synthetic `PacketLogger` traces, which name no channel in a connection request.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use pods_proto::dictation;
use pods_proto::packets::CONTROL_PSM;
use pods_proto::sdulog::Direction;

use super::super::fixture::{
    ATT_CIDS, CONTROL_CIDS, DICTATION_START, KEYBOARD_HANDLE, PKLG_ACL_RECEIVED, PKLG_COUNTED_LEN,
    PKLG_SCO_RECEIVED, PKLG_SCO_SENT, PODS_ADDRESS, PODS_HANDLE, SIGNALLING_CIDS, Trace,
    VOICE_BYTES, audio_notification, mac_accessory_packets, mac_att_packets, mac_session, session,
};
use super::{
    OWNER_FILE, PERMISSION_BITS, RUN, assert_holds_no_secret, files, hci, holds, holds_address,
    home_with_trace, log, names_in, run_dir, spellings,
};

/// Every file a run of the macOS trace holds: no music table, since no link opened PSM 0x1001.
pub(super) const MAC_FILES: [&str; 6] = [
    "att.log",
    "links.txt",
    "marks.tsv",
    "meta.txt",
    "psm.tsv",
    "sdu.log",
];
/// The whole of `links.txt` for the macOS trace.
const MAC_LINKS: &str = "synchronous_packets_from_host 1\n\
    synchronous_packets_from_pods 2\n\
    realtime_at_zero_ns 1795176000004000000\n\
    stamps_stepping_back 0\n\
    synchronous_connection_events 1\n\
    synchronous_connections_opened 1\n\
    acl_packets 19\n\
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
    avdtp_channels_on_other_links 0\n\
    unmapped_channel handle=0x000c cid=0x0040 sent_by=pods pdus=1\n\
    unmapped_channel handle=0x000b cid=0x0052 sent_by=host pdus=1\n\
    unmapped_channel handle=0x000b cid=0x0053 sent_by=host pdus=2\n\
    channel_by_content handle=0x000b cid=0x0050 sent_by=host carries=accessory pdus=3\n\
    channel_by_content handle=0x000b cid=0x0040 sent_by=pods carries=accessory pdus=2\n\
    channel_by_content handle=0x000b cid=0x0041 sent_by=pods carries=attributes pdus=5\n\
    channel_by_content handle=0x000b cid=0x0051 sent_by=host carries=attributes pdus=3\n\
    log sdu.log handle=0x000b written=5 refused_empty=0 refused_too_long=0\n\
    log att.log handle=0x000b written=8 refused_empty=0 refused_too_long=0\n\
    music_tables_not_written reason=no_link_opened_psm_0x1001\n";
/// The table of connection requests when the trace holds none.
const NO_REQUESTS: &str =
    "at_ns\tside\tpsm\tresult\thandle\thost_mtu\tpods_mtu\thost_mode\tpods_mode\n";
/// What every refusal of a `PacketLogger` trace ends with.
const RULE: &str = "; a file that does not start with the btsnoop identification pattern is read as a PacketLogger trace, which is refused unless it parses to its last byte\n";
/// The length, the seconds, the microseconds and the type in front of every payload.
const RECORD_HEADER: usize = 13;
/// The length field, which opens every record.
const LENGTH_FIELD: usize = 4;
/// An accessory packet that is not the first on its channel in the traces below.
const ACCESSORY_PACKET: [u8; 7] = [0x04, 0x00, 0x04, 0x00, 0x4b, 0x00, 0x03];
/// A packet no content rule reads.
const OTHER_PACKET: [u8; 3] = [0x08, 0x09, 0x0a];
/// The most identifiers one trace lists, mapped or not, written out so the test pins the number.
const MOST_LISTED: usize = 1024;
/// The first of a run of identifiers no fixture trace uses.
const SPARE_CID: u16 = 0x0100;

// Sample input: `0e 00 00 00`, then seconds, microseconds and a type, then 5 payload bytes: the length counts 9 bytes more than the payload, little endian.
fn record_offsets(trace: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut rest = trace;
    while !rest.is_empty() {
        offsets.push(trace.len().checked_sub(rest.len()).unwrap());
        let counted = u32::from_le_bytes(rest[..LENGTH_FIELD].try_into().unwrap());
        let payload = usize::try_from(counted.checked_sub(PKLG_COUNTED_LEN).unwrap()).unwrap();
        rest = &rest[RECORD_HEADER.checked_add(payload).unwrap()..];
    }
    offsets
}

/// Runs the command on a trace it must refuse and returns what it said, after checking that it made nothing.
fn refusal(trace: &[u8]) -> String {
    let (home, path) = home_with_trace(trace);
    let refused = hci(&home, &path, RUN);
    assert!(!refused.status.success(), "{refused:?}");
    assert_eq!(refused.stdout, b"");
    assert!(!home.path().join("omapods-captures").exists());
    let said = String::from_utf8(refused.stderr).unwrap();
    let prefix = format!("xtask probe: {}: ", path.display());
    said.strip_prefix(&prefix).unwrap().to_owned()
}

/// The lines of `links.txt` that name a channel, mapped or not, and the names of the files written.
fn mapping(trace: &Trace) -> (Vec<String>, Vec<String>) {
    let (home, path) = home_with_trace(&trace.bytes());
    let ran = hci(&home, &path, RUN);
    assert!(ran.status.success(), "{ran:?}");
    let links = String::from_utf8(ran.stdout).unwrap();
    let named = |line: &&str| {
        line.starts_with("unmapped_channel ") || line.starts_with("channel_by_content ")
    };
    let channels = links.lines().filter(named).map(str::to_owned).collect();
    (channels, names_in(&run_dir(&home, RUN)))
}

#[test]
fn a_packet_logger_trace_exports_and_its_logs_match_the_planted_packets() {
    let trace = mac_session().bytes();
    let (home, path) = home_with_trace(&trace);
    let ran = hci(&home, &path, RUN);
    assert!(ran.status.success(), "{ran:?}");
    assert_eq!(String::from_utf8(ran.stderr).unwrap(), "");
    let dir = run_dir(&home, RUN);

    assert_eq!(names_in(&dir), MAC_FILES);
    assert_eq!(log(&dir, "sdu.log"), mac_accessory_packets());
    assert_eq!(log(&dir, "att.log"), mac_att_packets());
    let links = fs::read_to_string(dir.join("links.txt")).unwrap();
    assert_eq!(String::from_utf8(ran.stdout).unwrap(), links);
    assert_eq!(links, MAC_LINKS);
    let requests = fs::read_to_string(dir.join("psm.tsv")).unwrap();
    assert_eq!(requests, NO_REQUESTS);
    for name in MAC_FILES {
        let mode = fs::metadata(dir.join(name)).unwrap().permissions().mode();
        assert_eq!(mode & PERMISSION_BITS, OWNER_FILE, "{name}");
    }
    assert_eq!(fs::read(&path).unwrap(), trace);
}

#[test]
fn a_truncated_record_fails_with_its_offset_and_no_run_is_made() {
    let trace = mac_session().bytes();
    let offsets = record_offsets(&trace);
    let last = offsets.len() - 1;
    for (cut_at, record) in [
        (trace.len() - 1, last),
        (offsets[last] + RECORD_HEADER, last),
        (offsets[1] + RECORD_HEADER + 1, 1),
    ] {
        let said = format!(
            "PacketLogger record {record} at byte offset {} is cut short{RULE}",
            offsets[record]
        );
        assert_eq!(refusal(&trace[..cut_at]), said, "cut at {cut_at}");
    }

    let mut big_endian = trace.clone();
    big_endian[..LENGTH_FIELD].reverse();
    let said = format!("PacketLogger record 0 at byte offset 0 is cut short{RULE}");
    assert_eq!(refusal(&big_endian), said);
}

#[test]
fn residue_after_the_last_record_is_refused() {
    let trace = mac_session().bytes();
    let records = record_offsets(&trace).len();
    for residue in [1, RECORD_HEADER - 1] {
        let mut with_residue = trace.clone();
        with_residue.extend(vec![0x00; residue]);
        let said = format!(
            "PacketLogger record {records} at byte offset {} is residue: fewer bytes than the 13 of a length, a stamp and a type{RULE}",
            trace.len()
        );
        assert_eq!(refusal(&with_residue), said, "{residue} bytes");
    }

    for counted in [0, PKLG_COUNTED_LEN - 1] {
        let mut short = trace.clone();
        short.extend(counted.to_le_bytes());
        short.extend([0x00; RECORD_HEADER - LENGTH_FIELD]);
        let said = format!(
            "PacketLogger record {records} at byte offset {} claims a length under the 9 bytes of its stamp and its type{RULE}",
            trace.len()
        );
        assert_eq!(refusal(&short), said, "a length of {counted}");
    }

    let said = format!("PacketLogger trace holds no record{RULE}");
    assert_eq!(refusal(&[]), said);
    let mut not_quite_btsnoop = session().bytes();
    not_quite_btsnoop[7] = b'1';
    let said = format!("PacketLogger record 0 at byte offset 0 is cut short{RULE}");
    assert_eq!(refusal(&not_quite_btsnoop), said);
}

#[test]
fn a_trace_without_connection_requests_is_mapped_by_content() {
    let (host, pods) = (Direction::ToPods, Direction::FromPods);
    let mut trace = Trace::packet_logger();
    trace.send(host, CONTROL_CIDS, &ACCESSORY_PACKET);
    trace.send(pods, CONTROL_CIDS, &ACCESSORY_PACKET);
    trace.send(pods, CONTROL_CIDS, &OTHER_PACKET);
    // What mapped a half stands: audio on the accessory half moves it to no other log.
    trace.send(pods, CONTROL_CIDS, &audio_notification(1));
    trace.send(host, ATT_CIDS, &OTHER_PACKET);
    trace.send(host, ATT_CIDS, &DICTATION_START);
    trace.send(pods, ATT_CIDS, &OTHER_PACKET);
    trace.send(pods, ATT_CIDS, &audio_notification(0));
    let (lines, names) = mapping(&trace);
    assert_eq!(
        lines,
        [
            "channel_by_content handle=0x000b cid=0x0050 sent_by=host carries=accessory pdus=1",
            "channel_by_content handle=0x000b cid=0x0040 sent_by=pods carries=accessory pdus=3",
            "channel_by_content handle=0x000b cid=0x0051 sent_by=host carries=attributes pdus=2",
            "channel_by_content handle=0x000b cid=0x0041 sent_by=pods carries=attributes pdus=2",
        ]
    );
    assert_eq!(names, MAC_FILES);
}

#[test]
fn a_channel_that_matches_nothing_is_unmapped_not_guessed() {
    let (host, pods) = (Direction::ToPods, Direction::FromPods);
    let mut trace = Trace::packet_logger();
    // An accessory packet that is not the first on its identifier, the start write from the pods and audio from the host map nothing.
    trace.send(host, CONTROL_CIDS, &OTHER_PACKET);
    trace.send(host, CONTROL_CIDS, &ACCESSORY_PACKET);
    trace.send(pods, ATT_CIDS, &DICTATION_START);
    trace.send(host, ATT_CIDS, &audio_notification(0));
    trace.send(pods, SIGNALLING_CIDS, &dictation::STARTED);
    let (lines, names) = mapping(&trace);
    assert_eq!(
        lines,
        [
            "unmapped_channel handle=0x000b cid=0x0050 sent_by=host pdus=2",
            "unmapped_channel handle=0x000b cid=0x0041 sent_by=pods pdus=1",
            "unmapped_channel handle=0x000b cid=0x0051 sent_by=host pdus=1",
            "unmapped_channel handle=0x000b cid=0x0042 sent_by=pods pdus=1",
        ]
    );
    assert_eq!(names, ["links.txt", "marks.tsv", "meta.txt", "psm.tsv"]);
}

#[test]
fn what_content_mapped_takes_the_log_number_after_what_a_request_opened() {
    let (host, pods) = (Direction::ToPods, Direction::FromPods);
    let mut trace = Trace::packet_logger();
    trace.open(host, CONTROL_PSM, CONTROL_CIDS);
    trace.send(host, CONTROL_CIDS, &OTHER_PACKET);
    trace.on(KEYBOARD_HANDLE);
    trace.send(pods, ATT_CIDS, &ACCESSORY_PACKET);
    let (home, path) = home_with_trace(&trace.bytes());
    let ran = hci(&home, &path, RUN);
    assert!(ran.status.success(), "{ran:?}");
    let dir = run_dir(&home, RUN);
    assert_eq!(
        names_in(&dir),
        [
            "a2dp.tsv",
            "avdtp.txt",
            "links.txt",
            "marks.tsv",
            "meta.txt",
            "psm.tsv",
            "sdu-2.log",
            "sdu.log"
        ]
    );
    assert_eq!(log(&dir, "sdu.log"), [(host, OTHER_PACKET.to_vec())]);
    assert_eq!(log(&dir, "sdu-2.log"), [(pods, ACCESSORY_PACKET.to_vec())]);
    let links = String::from_utf8(ran.stdout).unwrap();
    let said = "channel_by_content handle=0x000c cid=0x0041 sent_by=pods carries=accessory pdus=1\n\
        log sdu.log handle=0x000b written=1 refused_empty=0 refused_too_long=0\n\
        log sdu-2.log handle=0x000c written=1 refused_empty=0 refused_too_long=0\n\
        music a2dp.tsv avdtp.txt handle=0x000b ";
    assert!(links.contains(said), "{links}");
}

#[test]
fn each_link_is_mapped_on_its_own_and_halves_past_the_limit_are_counted() {
    let (host, pods) = (Direction::ToPods, Direction::FromPods);
    let mut trace = Trace::packet_logger();
    trace.send(host, CONTROL_CIDS, &ACCESSORY_PACKET);
    trace.end_link(PODS_HANDLE);
    // The handle is given out again, to a link whose first packet on the same identifier is not an accessory packet.
    trace.send(host, CONTROL_CIDS, &OTHER_PACKET);
    trace.send(pods, CONTROL_CIDS, &ACCESSORY_PACKET);
    for cid in (SPARE_CID..).take(MOST_LISTED) {
        trace.pdu(pods, cid, &OTHER_PACKET);
    }
    let (home, path) = home_with_trace(&trace.bytes());
    let ran = hci(&home, &path, RUN);
    assert!(ran.status.success(), "{ran:?}");
    let dir = run_dir(&home, RUN);
    assert_eq!(log(&dir, "sdu.log"), [(host, ACCESSORY_PACKET.to_vec())]);
    assert_eq!(log(&dir, "sdu-2.log"), [(pods, ACCESSORY_PACKET.to_vec())]);
    let links = String::from_utf8(ran.stdout).unwrap();
    // Three halves were listed before the spare identifiers, so that many of those are past the limit.
    assert!(links.contains("\nunmapped_over_limit 3\n"), "{links}");
    let unmapped = "unmapped_channel handle=0x000b cid=0x0050 sent_by=host pdus=1\n";
    assert!(links.contains(unmapped), "{links}");
    let listed = links.lines().filter(|line| line.contains(" cid=")).count();
    assert_eq!(listed, MOST_LISTED);
}

#[test]
fn an_unmapped_identifier_is_listed_once_when_its_handle_is_given_out_again() {
    let (host, pods) = (Direction::ToPods, Direction::FromPods);
    let mut trace = Trace::packet_logger();
    trace.send(host, CONTROL_CIDS, &OTHER_PACKET);
    trace.end_link(PODS_HANDLE);
    trace.send(host, CONTROL_CIDS, &OTHER_PACKET);
    trace.send(host, CONTROL_CIDS, &OTHER_PACKET);
    // The same identifier from the other side, or on another handle, is another line.
    trace.pdu(pods, CONTROL_CIDS.1, &OTHER_PACKET);
    trace.on(KEYBOARD_HANDLE);
    trace.send(host, CONTROL_CIDS, &OTHER_PACKET);
    let (lines, names) = mapping(&trace);
    // One line per handle, identifier and direction, as for a trace the signalling maps: two lines alike could not be told apart.
    assert_eq!(
        lines,
        [
            "unmapped_channel handle=0x000b cid=0x0050 sent_by=host pdus=3",
            "unmapped_channel handle=0x000b cid=0x0050 sent_by=pods pdus=1",
            "unmapped_channel handle=0x000c cid=0x0050 sent_by=host pdus=1",
        ]
    );
    assert_eq!(names, ["links.txt", "marks.tsv", "meta.txt", "psm.tsv"]);
}

#[test]
fn output_of_a_packet_logger_trace_holds_no_address() {
    let trace = mac_session().bytes();
    assert!(holds_address(&trace, PODS_ADDRESS));
    assert!(holds(&trace, VOICE_BYTES));
    let holds_none = |what: &str, bytes: &[u8]| {
        assert_holds_no_secret(what, bytes);
        for spelling in spellings(VOICE_BYTES) {
            assert!(!holds(bytes, &spelling), "{what} holds synchronous data");
        }
    };
    let (home, path) = home_with_trace(&trace);
    let ran = hci(&home, &path, RUN);
    assert!(ran.status.success(), "{ran:?}");
    holds_none("stdout", &ran.stdout);
    holds_none("stderr", &ran.stderr);
    let written = files(&run_dir(&home, RUN));
    assert_eq!(written.len(), MAC_FILES.len());
    for (name, contents) in &written {
        holds_none(name, contents);
    }

    let (home, path) = home_with_trace(&trace[..trace.len() - 1]);
    let refused = hci(&home, &path, RUN);
    assert!(!refused.status.success(), "{refused:?}");
    holds_none("stdout of a refusal", &refused.stdout);
    holds_none("stderr of a refusal", &refused.stderr);
}

#[test]
fn sco_records_are_counted_and_hold_no_audio() {
    let mut trace = Trace::packet_logger();
    for kind in [PKLG_SCO_SENT, PKLG_SCO_SENT, PKLG_SCO_RECEIVED] {
        trace.logged(kind, VOICE_BYTES);
    }
    // The types Wireshark knows as synchronous data, 0x08 and 0x09, are not the ones macOS writes with the audio left out.
    for kind in [0x08, 0x09, PKLG_SCO_SENT - 1, PKLG_SCO_RECEIVED + 1] {
        trace.logged(kind, VOICE_BYTES);
    }
    trace.logged(PKLG_ACL_RECEIVED, &[]);
    let (home, path) = home_with_trace(&trace.bytes());
    let ran = hci(&home, &path, RUN);
    assert!(ran.status.success(), "{ran:?}");
    let links = String::from_utf8(ran.stdout).unwrap();
    let counted = "synchronous_packets_from_host 2\n\
        synchronous_packets_from_pods 1\n\
        realtime_at_zero_ns 1795176000000000000\n";
    assert!(links.starts_with(counted), "{links}");
    assert!(
        links.contains("\nacl_packets 1\nmalformed_acl_packets 1\n"),
        "{links}"
    );
    for (name, contents) in &files(&run_dir(&home, RUN)) {
        for spelling in spellings(VOICE_BYTES) {
            assert!(!holds(contents, &spelling), "{name} holds synchronous data");
        }
    }

    let btsnoop = session().bytes();
    let (home, path) = home_with_trace(&btsnoop);
    let ran = hci(&home, &path, RUN);
    assert!(ran.status.success(), "{ran:?}");
    assert!(!holds(&ran.stdout, b"synchronous_packets"), "{ran:?}");
}
