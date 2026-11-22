//! `probe analyse` on the dictation runs that `probe capture` leaves when it records the fake pods.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use pods_proto::dictation::DictationPacket;
use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
use tempfile::TempDir;

use super::super::att_pods::{AttPods, BURST_AFTER, BURST_SPAN, Pace, TABLE_END};
use super::super::pods::{self, Pods, Transcript};
use super::{
    FAILED, HANG_LIMIT, Running, capture, far_end, files_of, hand_over, home_with_device, probe,
    read, route, run_path, transcript,
};

/// How long the capture of a paced stream may take: past the minute a clock figure needs, well short of the two its soak is asked for.
const PACED_LIMIT: Duration = Duration::from_secs(90);
/// The frame length and the lead the paced pods are told, the ones a dictation session was seen to have.
const SEEN_PACE: Pace = Pace {
    frame: Duration::from_millis(20),
    lead: Duration::from_millis(930),
};
/// The steady packets of a paced capture: 61 s of frames, so the minute a clock figure needs is in the log.
const PAST_A_MINUTE: u32 = 3050;
/// The steady packets of a short capture, a fifth of a second of the unpaced stream.
const A_FEW: u32 = 100;
/// How far a time of the paced capture may lie from what the pods were told: half a frame, so a figure one 20 ms frame off fails.
const HALF_A_FRAME_MS: f64 = 10.0;
/// The sample clock a 20 ms frame of 320 samples runs at.
const REFERENCE_HZ: f64 = 16_000.0;
const MS_PER_SECOND: f64 = 1000.0;
/// How close its rates must come, as a part of the rate: one frame more or less among the 3050 is 328 parts in a million.
const HALF_A_FRAME_IN_THE_SPAN: f64 = 160e-6;

/// A dictation soak captured from fake pods playing `att` until they raise their flag, then analysed by the built command.
struct Analysed {
    /// Holds the temporary home until the test ends.
    _home: TempDir,
    run: PathBuf,
    pods: Transcript,
    /// The rows of `analysis-att.tsv`, each `key\tvalue\tunit\tnote`.
    rows: Vec<Vec<String>>,
}

impl Analysed {
    fn of(att: AttPods, limit: Duration) -> Self {
        let home = home_with_device();
        let (control, control_far) = hand_over();
        let (dialled, att_far) = hand_over();
        let script = ["--route", "dictation", "--script", "soak", "--minutes", "2"];
        let command = capture(&home, "dictation", &script);
        let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, dialled)];
        let running = Running::start(command, routes);
        let flag = Arc::new(AtomicBool::new(false));
        let part = Pods {
            att,
            deadline: limit,
            ..Pods::default()
        };
        let control_far = Some(far_end(&control_far));
        let played = pods::spawn_dialled(control_far, att_far, part, Arc::clone(&flag));
        running.interrupt_within(limit, &flag);
        let ran = running.finish();
        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let pods = transcript(&played);
        let run = run_path(&home, "dictation");
        let mut analyse = probe(home.path(), &["analyse", run.to_str().unwrap()]);
        let analysed = analyse.output().unwrap();
        let marks = read(&run, "marks.tsv");
        assert!(analysed.status.success(), "{analysed:?}\n{marks}");
        let table = read(&run, "analysis-att.tsv");
        let printed = String::from_utf8(analysed.stdout).unwrap();
        assert!(printed.ends_with(&table), "{printed}");
        let fields = |line: &str| line.split('\t').map(str::to_owned).collect();
        Self {
            _home: home,
            run,
            pods,
            rows: table.lines().map(fields).collect(),
        }
    }

    /// The value of the one row with `key`.
    fn value(&self, key: &str) -> &str {
        let mut found = self.rows.iter().filter(|row| row[0] == key);
        let row = found.next().unwrap_or_else(|| panic!("no row {key}"));
        assert!(found.next().is_none(), "two rows {key}");
        &row[1]
    }

    fn number(&self, key: &str) -> f64 {
        self.value(key).parse().unwrap()
    }

    /// How many frames the fake pods sent, in every audio notification.
    fn frames_sent(&self) -> usize {
        let sent = self.pods.att_sent.iter();
        sent.map(|pdu| match DictationPacket::parse(pdu) {
            Ok(Some(DictationPacket::Audio(audio))) => audio.frames().count(),
            _ => 0,
        })
        .sum()
    }
}

#[test]
#[ignore = "a minute at real time; the slow job runs it"]
fn a_paced_dictation_capture_analyses_to_the_clock_lead_and_burst_the_pods_kept() {
    let att = AttPods {
        pace: Some(SEEN_PACE),
        interrupt_after: Some(PAST_A_MINUTE),
        ..AttPods::default()
    };
    let found = Analysed::of(att, PACED_LIMIT);

    let ms = |told: Duration| told.as_secs_f64() * MS_PER_SECOND;
    let frame_rate = found.number("frame_rate");
    let off = (frame_rate * SEEN_PACE.frame.as_secs_f64() - 1.0).abs();
    assert!(off < HALF_A_FRAME_IN_THE_SPAN, "{frame_rate}");
    let clock_hz = found.number("clock_hz");
    let off = (clock_hz / REFERENCE_HZ - 1.0).abs();
    assert!(off < HALF_A_FRAME_IN_THE_SPAN, "{clock_hz}");
    assert!(found.number("clock_hz_low") <= clock_hz);
    assert!(found.number("clock_hz_high") >= clock_hz);
    // No frame leaves the pods before it has ended, so the lead is never over the one they were told; it falls short by their latency.
    let (lead_ms, told_ms) = (found.number("lead_ms"), ms(SEEN_PACE.lead));
    assert!(lead_ms <= told_ms, "{lead_ms}");
    assert!(lead_ms > told_ms - HALF_A_FRAME_MS, "{lead_ms}");
    // The start write is stamped before it is sent and the pods wait from its receipt, so the first audio is never early.
    let (audio_after_ms, told_ms) = (found.number("start_to_audio_ms"), ms(BURST_AFTER));
    assert!(audio_after_ms >= told_ms, "{audio_after_ms}");
    assert!(
        audio_after_ms < told_ms + HALF_A_FRAME_MS,
        "{audio_after_ms}"
    );
    assert_eq!(found.value("burst_packets"), "13");
    assert_eq!(found.value("burst_frames"), "63");
    // Both ends of the burst are logged late by their own latency, so its length may fall on either side.
    let (burst_ms, told_ms) = (found.number("burst_ms"), ms(BURST_SPAN));
    assert!((burst_ms - told_ms).abs() < HALF_A_FRAME_MS, "{burst_ms}");
    assert_eq!(found.value("frame_counter_skips"), "0");
    assert_eq!(found.value("packet_counter_skips"), "0");
    assert_eq!(found.value("parse_failures"), "0");
    assert!(found.number("bitrate_mean") > 0.0);
    let frames = found.frames_sent();
    assert_eq!(found.value("audio_frames"), frames.to_string());
}

#[test]
fn a_short_dictation_capture_refuses_its_rates_and_keeps_every_frame_and_layout_row() {
    let att = AttPods {
        interrupt_after: Some(A_FEW),
        ..AttPods::default()
    };
    let found = Analysed::of(att, HANG_LIMIT);

    let files = "analysis-att.tsv analysis.tsv att-handles.tsv att.log frames.tsv handles.tsv marks.tsv meta.txt sdu.log units.tsv";
    assert_eq!(files_of(&found.run).join(" "), files);
    let rates = "frame_rate clock_hz clock_hz_low clock_hz_high clock_ppm_vs_16000 bitrate_mean bitrate_second_min bitrate_second_median bitrate_second_max";
    for key in rates.split(' ') {
        let row = found.rows.iter().find(|row| row[0] == key).unwrap();
        assert_eq!(row[1], "refused", "{row:?}");
        assert!(row[3].ends_with("needs 500 over 60 s"), "{row:?}");
    }
    let frames = found.frames_sent();
    assert!(frames > usize::try_from(A_FEW).unwrap(), "{frames}");
    let listed = read(&found.run, "frames.tsv");
    let header = "index\tpacket\tarrival_ns\tsession\tcounter\tflag\tlevel\tlen\ttoc\n";
    assert!(listed.starts_with(header), "{listed}");
    assert_eq!(listed.lines().count(), frames + 1, "{listed}");
    assert_eq!(found.value("audio_frames"), frames.to_string());
    assert_eq!(found.value("frame_counter_skips"), "0");
    assert_eq!(found.value("packet_counter_skips"), "0");
    assert_eq!(found.value("parse_failures"), "0");
    assert_eq!(found.value("unexpected_codec"), "0");
    assert!(found.value("frames_per_packet").contains("4:2 5:11"));
    assert_ne!(found.value("lead_ms"), "refused");
    assert_ne!(found.value("start_to_reply_ms"), "refused");
    assert_ne!(found.value("stop_to_reply_ms"), "refused");
    let declared = read(&found.run, "att-handles.tsv");
    assert_eq!(declared.lines().count(), usize::from(TABLE_END) + 1);
}

#[test]
fn a_frame_the_pods_withhold_is_one_frame_counter_skip_in_the_capture() {
    let att = AttPods {
        withheld: Some(100),
        interrupt_after: Some(A_FEW),
        ..AttPods::default()
    };
    let found = Analysed::of(att, HANG_LIMIT);

    assert_eq!(found.value("frame_counter_skips"), "1");
    assert_eq!(found.value("frame_counter_skip"), "99 to 101");
    assert_eq!(found.value("packet_counter_skips"), "0");
    assert_eq!(found.value("parse_failures"), "0");
    assert_eq!(found.value("audio_frames"), found.frames_sent().to_string());
}
