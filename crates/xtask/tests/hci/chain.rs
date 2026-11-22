//! The built binary's commands in a row on a synthetic `PacketLogger` trace: `probe hci`, then `analyse`, `decode-opus` and `audio` on the run it made, with no capture.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use pods_proto::dictation;
use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
use pods_proto::sdulog::Direction;
use tempfile::TempDir;

use super::super::dictation::{DICTATION, FRAME, SAMPLE_BYTES, near_zero_percent};
use super::super::fixture::{
    ATT_CIDS, CONTROL_CIDS, DEVICE_NAME, PODS_ADDRESS, PODS_HANDLE, START_UNIX_MICROS, Trace,
    mac_naming, mac_opening,
};
use super::super::oracles::opus_oracles_installed;
use super::super::run::{probe, row, rows};
use super::super::synth::{
    AUDIO_AFTER_MS, NS_PER_SECOND, Plan, REPLY_AFTER_MS, SAMPLES_PER_FRAME, Session, synth,
};
use super::pklg::MAC_FILES;
use super::{
    ENHANCED_RETRANSMISSION, I_FRAME, RUN, assert_holds_no_secret, files, hci, holds,
    holds_address, home_with_trace, names_in, run_dir,
};

/// A trace stamps its records in microseconds.
const NS_PER_MICRO: u64 = 1000;
/// When the first session starts, and how long after the stop write of one the next starts.
const START_NS: u64 = 2 * NS_PER_SECOND;
const BETWEEN_NS: u64 = 3 * NS_PER_SECOND;
/// The first record the reader keeps is the fourth of the opening, this far into the trace, and the stamps of a run count from it.
const ZERO_AFTER_NS: u64 = 4_000_000;
/// The frames of the two sessions: one long enough for its rates, 61 s after its burst, and one too short for them.
const FRAMES: [u32; 2] = [3113, 400];
/// The packets the opening burst of a session comes in: its frames by four, then by fives, then what is left.
const BURST_PACKETS: u32 = 13;
/// The field of a line of the frames table that holds its session, counted from 0.
const SESSION_FIELD: usize = 3;
/// The line of `links.txt` that says the accessory log was written empty, and why.
const EMPTY_LOG: &str = "accessory_log_written_empty reason=no_accessory_channel_found_and_analyse_opens_sdu.log_beside_att.log";
/// The frame counter the second trace never sends.
const WITHHELD: u16 = 100;

/// Sessions in a row from their frame counts and the frame counter each withholds, the packet counter running on from one into the next.
fn sessions_of(planned: &[(u32, Option<u16>)]) -> Vec<Session> {
    let mut made: Vec<Session> = Vec::new();
    for &(frames, withheld) in planned {
        let (start_ns, first_packet) = made.last().map_or((START_NS, 0), |last| {
            (last.stop_ns.saturating_add(BETWEEN_NS), last.next_packet)
        });
        made.push(synth(Plan {
            first_packet,
            withheld,
            ..Plan::seen(start_ns, frames)
        }));
    }
    made
}

/// One packet of the attribute channel: its stamp, who sent it and its bytes.
type Packet<'a> = (u64, Direction, &'a [u8]);

/// What macOS would write when the packets of the attribute channel follow what `trace` opens with, each at its stamp to the microsecond a trace keeps.
fn trace_after(mut trace: Trace, packets: &[Packet<'_>]) -> Vec<u8> {
    for &(at_ns, direction, packet) in packets {
        trace.at_micros(at_ns / NS_PER_MICRO);
        trace.send(direction, ATT_CIDS, packet);
    }
    trace.end_link(PODS_HANDLE);
    trace.bytes()
}

/// The trace of `packets` after an opening that holds the accessory packets.
fn trace_of(packets: &[Packet<'_>]) -> Vec<u8> {
    trace_after(mac_opening(), packets)
}

fn packets_of(sessions: &[Session]) -> Vec<Packet<'_>> {
    let records = sessions.iter().flat_map(|session| &session.records);
    records
        .map(|record| (record.at_ns, record.direction, &record.sdu[..]))
        .collect()
}

fn trace_of_sessions(sessions: &[Session]) -> Vec<u8> {
    trace_of(&packets_of(sessions))
}

/// What a command of the chain wrote on its standard output, after checking that it succeeded, said nothing else and named nothing private.
fn printed(what: &str, ran: &Output) -> String {
    assert!(ran.status.success(), "{what}: {ran:?}");
    assert_eq!(String::from_utf8(ran.stderr.clone()).unwrap(), "", "{what}");
    assert_holds_no_secret(what, &ran.stdout);
    String::from_utf8(ran.stdout.clone()).unwrap()
}

/// What a command of the chain said when it refused, after checking that it printed nothing and named nothing private.
fn refused(what: &str, ran: &Output) -> String {
    assert_eq!(ran.status.code(), Some(1), "{what}: {ran:?}");
    assert_eq!(ran.stdout, b"", "{what}");
    assert_holds_no_secret(what, &ran.stderr);
    String::from_utf8(ran.stderr.clone()).unwrap()
}

/// What a command says of a file of `run` it needs and the run does not hold.
fn not_held(run: &Path, name: &str) -> String {
    format!(
        "xtask probe: could not open {}: No such file or directory (os error 2)\n",
        run.join(name).display()
    )
}

/// Runs `probe hci` and then `probe analyse` on `trace`, which names the pods, and returns the home, the run and what `analyse` printed.
fn analysed(trace: &[u8]) -> (TempDir, PathBuf, String) {
    assert!(holds_address(trace, PODS_ADDRESS));
    assert!(holds(trace, DEVICE_NAME.as_bytes()));
    let (home, path) = home_with_trace(trace);
    printed("hci", &hci(&home, &path, RUN));
    let run = run_dir(&home, RUN);
    let table = printed("analyse", &probe(&home, "analyse", &run).output().unwrap());
    for (name, contents) in files(&run) {
        assert_holds_no_secret(&name, &contents);
    }
    (home, run, table)
}

/// The value of a row every session has, session by session.
fn per_session<'a>(table: &'a str, key: &str) -> Vec<&'a str> {
    rows(table, key).iter().map(|row| row.0).collect()
}

/// The share of `samples` near zero as a table prints it, in percent with two places.
fn near_zero(samples: &[f32]) -> String {
    format!("{:.2}", near_zero_percent(samples))
}

// Sample input: `7\t3\t2080000000\t1\t7\t0x80\t213\t81\t0xb8`, a frame of session 1 by its fourth field; the first line of the table is its header.
fn listed_in(table: &str, session: u32) -> usize {
    let wanted = session.to_string();
    let sessions = table
        .lines()
        .skip(1)
        .map(|line| line.split('\t').nth(SESSION_FIELD));
    sessions
        .filter(|found| *found == Some(wanted.as_str()))
        .count()
}

#[test]
fn a_trace_is_analysed_with_no_capture_and_gives_the_planted_sessions() {
    let sessions = sessions_of(&[(FRAMES[0], None), (FRAMES[1], None)]);
    let (_home, run, table) = analysed(&trace_of_sessions(&sessions));
    assert_eq!(row(&table, "script").0, "trace");
    let meta = format!(
        "script=trace\ntool=xtask probe hci {}\nrealtime_at_zero_ns={}\n",
        env!("CARGO_PKG_VERSION"),
        (START_UNIX_MICROS * NS_PER_MICRO) + ZERO_AFTER_NS
    );
    assert_eq!(fs::read_to_string(run.join("meta.txt")).unwrap(), meta);
    let seen = Plan::seen(START_NS, FRAMES[0]);
    // A notification for every packet of the burst, then one for every frame after it.
    let notifications = FRAMES.map(|frames| BURST_PACKETS + frames - seen.burst_frames);
    for (key, planted) in [
        ("audio_notifications", notifications[0] + notifications[1]),
        ("audio_frames", FRAMES[0] + FRAMES[1]),
        ("parse_failures", 0),
        ("length_mismatches", 0),
        ("packet_counter_skips", 0),
        ("frame_counter_skips", 0),
        ("unexpected_codec", 0),
        ("audio_outside_a_session", 0),
    ] {
        assert_eq!(row(&table, key).0, planted.to_string(), "{key}\n{table}");
    }
    for (key, planted) in [
        ("session_audio_notifications", notifications),
        ("session_frames", FRAMES),
        ("burst_packets", [BURST_PACKETS; 2]),
        ("burst_frames", [seen.burst_frames; 2]),
    ] {
        let planted = planted.map(|count| count.to_string());
        assert_eq!(per_session(&table, key), planted, "{key}\n{table}");
    }
    // The stamps of the generated sessions are whole microseconds, so each time comes back to the last place the table prints.
    for (key, planted_ms) in [
        ("start_to_reply_ms", REPLY_AFTER_MS),
        ("start_to_audio_ms", AUDIO_AFTER_MS),
        ("lead_ms", seen.lead_ms),
    ] {
        let planted = format!("{planted_ms}.000");
        assert_eq!(per_session(&table, key), [&planted; 2], "{key}\n{table}");
    }
    // Only the first session is long enough for a rate; the second says so in place of a number.
    let rate = format!("{}.000000", seen.clock_hz / SAMPLES_PER_FRAME);
    let rates = per_session(&table, "frame_rate");
    assert_eq!(rates, [rate.as_str(), "refused"], "{table}");

    let listed = fs::read_to_string(run.join("frames.tsv")).unwrap();
    for (session, planted) in (1..).zip(FRAMES) {
        let found = u32::try_from(listed_in(&listed, session)).unwrap();
        assert_eq!(found, planted, "session {session}");
    }
}

/// Runs `decode-opus` and `audio` on an analysed run of the two planted sessions and checks their tables and every file of the run.
fn assert_decodes_and_measures(home: &TempDir, run: &Path) {
    let decoded = probe(home, "decode-opus", run).output().unwrap();
    let decoded = printed("decode-opus", &decoded);
    let all = FRAMES[0].saturating_add(FRAMES[1]).to_string();
    for (key, planted) in [
        ("frames_in", all.as_str()),
        ("frames_not_b8", "0"),
        ("candidate_decoded", &all),
        ("candidate_errors", "0"),
        ("candidate_panics", "0"),
        ("libopus_refused", "0"),
        ("libopus_frames_compared", &all),
        ("ffopus_refused", "0"),
        ("ffopus_frames_compared", &all),
        ("rate_16k_candidate_decoded", &all),
        ("second_byte_removed_discriminates", "yes"),
    ] {
        assert_eq!(row(&decoded, key).0, planted, "{key}\n{decoded}");
    }

    let measured = probe(home, "audio", run).args(DICTATION).output().unwrap();
    let table = printed("audio", &measured);
    let written = fs::read_to_string(run.join("audio-dictation.tsv")).unwrap();
    assert_eq!(written, table);
    let wav = fs::read(run.join("dictation.f32.wav")).unwrap();
    let [first, second] =
        FRAMES.map(|frames| usize::try_from(frames).unwrap().saturating_mul(FRAME));
    let held = first.saturating_add(second);
    // The one data chunk ends the file decode-opus writes, so the samples are its last bytes.
    let data = wav.len().checked_sub(held.saturating_mul(SAMPLE_BYTES));
    let samples: Vec<f32> = wav[data.unwrap()..]
        .as_chunks::<SAMPLE_BYTES>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect();
    assert_eq!(row(&table, "stream_rate").0, "48000");
    assert_eq!(row(&table, "stream_samples").0, held.to_string());
    assert_eq!(row(&table, "whole_near_zero").0, near_zero(&samples));
    for (key, stretch) in [
        ("session_1", &samples[..first]),
        ("session_2", &samples[first..]),
    ] {
        let len = stretch.len().to_string();
        assert_eq!(row(&table, &format!("{key}_samples")).0, len, "{table}");
        let near = near_zero(stretch);
        assert_eq!(row(&table, &format!("{key}_near_zero")).0, near, "{table}");
        for measure in ["speech_rms", "floor", "cutoff", "reach"] {
            assert_eq!(
                rows(&table, &format!("{key}_{measure}")).len(),
                1,
                "{table}"
            );
        }
    }

    let written = files(run);
    let names: Vec<&str> = written.iter().map(|(name, _)| name.as_str()).collect();
    let all_files = "analysis-att.tsv analysis.tsv att-handles.tsv att.log audio-dictation.tsv decode-opus.tsv dictation.f32.wav dictation.opus ffopus.f32 ffopus.stderr frames.tsv libopus.f32 libopus.stderr links.txt marks.tsv meta.txt psm.tsv sdu.log units.tsv";
    assert_eq!(names.join(" "), all_files);
    for (name, contents) in &written {
        assert_holds_no_secret(name, contents);
    }
}

#[test]
fn the_whole_chain_runs_on_a_trace_and_no_output_holds_an_address() {
    if !opus_oracles_installed() {
        return;
    }
    let sessions = sessions_of(&[(FRAMES[0], None), (FRAMES[1], None)]);
    let (home, run, _table) = analysed(&trace_of_sessions(&sessions));
    assert_decodes_and_measures(&home, &run);
}

#[test]
fn a_trace_of_the_dictation_alone_runs_the_whole_chain_on_an_empty_accessory_log() {
    let sessions = sessions_of(&[(FRAMES[0], None), (FRAMES[1], None)]);
    // Recording started after the accessory exchange, so the trace holds no accessory packet at all.
    let trace = trace_after(mac_naming(), &packets_of(&sessions));
    let (home, run, table) = analysed(&trace);
    assert_eq!(fs::read_to_string(run.join("sdu.log")).unwrap(), "");
    let links = fs::read_to_string(run.join("links.txt")).unwrap();
    assert_eq!(links.lines().filter(|line| *line == EMPTY_LOG).count(), 1);
    assert_eq!(row(&table, "packets_from_pods").0, "0", "{table}");
    let all = (FRAMES[0] + FRAMES[1]).to_string();
    assert_eq!(row(&table, "audio_frames").0, all, "{table}");
    let planted = FRAMES.map(|frames| frames.to_string());
    assert_eq!(per_session(&table, "session_frames"), planted, "{table}");
    for key in ["parse_failures", "frame_counter_skips"] {
        assert_eq!(row(&table, key).0, "0", "{key}\n{table}");
    }
    if !opus_oracles_installed() {
        return;
    }
    assert_decodes_and_measures(&home, &run);
}

/// Exports `trace` and checks that the run holds `exported` alone, that no line of the summary names an accessory log and that the analysis refuses the run for want of one.
fn assert_no_accessory_log(trace: &[u8], exported: &[&str]) {
    let (home, path) = home_with_trace(trace);
    let links = printed("hci", &hci(&home, &path, RUN));
    assert!(!links.contains("sdu.log"), "{links}");
    let run = run_dir(&home, RUN);
    assert_eq!(names_in(&run), exported);
    let analyse = probe(&home, "analyse", &run).output().unwrap();
    assert_eq!(refused("analyse", &analyse), not_held(&run, "sdu.log"));
    assert_eq!(names_in(&run), exported);
}

#[test]
fn a_trace_with_neither_channel_gets_no_log_and_the_analysis_names_the_one_it_misses() {
    let exported = ["links.txt", "marks.tsv", "meta.txt", "psm.tsv"];
    assert_no_accessory_log(&trace_after(mac_naming(), &[]), &exported);
}

#[test]
fn an_accessory_channel_the_export_does_not_read_gets_no_empty_log_in_its_place() {
    // The accessory channel is open in a mode the export does not read, so the trace holds an accessory packet no log shows.
    let mut trace = Trace::new();
    trace.open(Direction::ToPods, CONTROL_PSM, CONTROL_CIDS);
    trace.offer_mode(Direction::FromPods, CONTROL_CIDS, ENHANCED_RETRANSMISSION);
    trace.open(Direction::FromPods, ATT_PSM, ATT_CIDS);
    trace.send(Direction::FromPods, CONTROL_CIDS, &I_FRAME);
    trace.send(Direction::FromPods, ATT_CIDS, &dictation::STARTED);
    let exported = [
        "a2dp.tsv",
        "att.log",
        "avdtp.txt",
        "links.txt",
        "marks.tsv",
        "meta.txt",
        "psm.tsv",
    ];
    assert_no_accessory_log(&trace.bytes(), &exported);
}

#[test]
fn a_frame_the_trace_never_carries_is_the_one_skip_the_analysis_counts() {
    let sessions = sessions_of(&[(FRAMES[1], Some(WITHHELD))]);
    let (_home, _run, table) = analysed(&trace_of_sessions(&sessions));
    assert_eq!(row(&table, "audio_frames").0, (FRAMES[1] - 1).to_string());
    assert_eq!(row(&table, "frame_counter_skips").0, "1", "{table}");
    let around = format!("{} to {}", WITHHELD - 1, WITHHELD + 1);
    assert_eq!(row(&table, "frame_counter_skip").0, around, "{table}");
    assert_eq!(row(&table, "packet_counter_skips").0, "0", "{table}");
    assert_eq!(row(&table, "parse_failures").0, "0", "{table}");
}

#[test]
fn a_trace_with_no_dictation_stream_is_refused_by_name_further_down_the_chain() {
    // No packet on the attribute channel: the run holds no log of it, so no frames table follows.
    let (home, run, table) = analysed(&trace_of(&[]));
    assert_eq!(row(&table, "script").0, "trace");
    assert_eq!(
        names_in(&run),
        [
            "analysis.tsv",
            "links.txt",
            "marks.tsv",
            "meta.txt",
            "psm.tsv",
            "sdu.log",
            "units.tsv"
        ]
    );
    assert_eq!(fs::read_to_string(run.join("marks.tsv")).unwrap(), "");
    let no_table = not_held(&run, "frames.tsv");
    let decode = probe(&home, "decode-opus", &run).output().unwrap();
    assert_eq!(refused("decode-opus", &decode), no_table);
    let audio = probe(&home, "audio", &run)
        .args(DICTATION)
        .output()
        .unwrap();
    assert_eq!(refused("audio", &audio), no_table);

    // The host starts and stops a session the pods answer and send no frame in.
    let asked = [
        (START_NS, Direction::ToPods, &dictation::START[..]),
        (
            START_NS + NS_PER_SECOND / 10,
            Direction::FromPods,
            &dictation::STARTED,
        ),
        (
            START_NS + NS_PER_SECOND,
            Direction::ToPods,
            &dictation::STOP,
        ),
    ];
    let (home, run, table) = analysed(&trace_of(&asked));
    assert_eq!(row(&table, "audio_frames").0, "0", "{table}");
    let (start_ns, stop_ns) = (
        START_NS - ZERO_AFTER_NS,
        START_NS + NS_PER_SECOND - ZERO_AFTER_NS,
    );
    let marks = format!(
        "{start_ns}\tdictation-start\tthe start write of the trace\n{stop_ns}\tdictation-stop\tthe stop write of the trace\n"
    );
    assert_eq!(fs::read_to_string(run.join("marks.tsv")).unwrap(), marks);
    let decode = probe(&home, "decode-opus", &run).output().unwrap();
    assert_eq!(
        refused("decode-opus", &decode),
        "xtask probe: att.log holds no frame, so there is nothing to decode\n"
    );
    let audio = probe(&home, "audio", &run)
        .args(DICTATION)
        .output()
        .unwrap();
    let no_decode = not_held(&run, "dictation.f32.wav");
    assert_eq!(refused("audio", &audio), no_decode);
    let exported: Vec<String> = names_in(&run)
        .into_iter()
        .filter(|name| MAC_FILES.contains(&name.as_str()))
        .collect();
    assert_eq!(exported, MAC_FILES);
}
