//! Opening and tail audio captured from the live dictation source.

use std::io::{self, Write};

use pods_proto::doap::doap_mode_on;

use super::super::super::support::recording::{dictation_marks, frames, rms_dbfs, span_of};
use super::*;

fn mode_set_at(fixture: &Fixture) -> Instant {
    let setting = doap_mode_on();
    wait_for("DoAP mode on", || {
        fixture
            .device
            .pods
            .heard()
            .into_iter()
            .find_map(|heard| (heard.sdu == setting).then_some(heard.at))
    })
}

#[test]
fn stalled_reader_keeps_the_quiet_opening_word() {
    const LEADING: u32 = 10;
    const QUIET: u32 = 2;
    const BURST: u32 = 2;
    const RELEASE: Duration = Duration::from_millis(221);
    const PAUSE_AT: Duration = Duration::from_millis(279);
    const STALL: Duration = Duration::from_millis(120);
    const PLAY_WORD: Duration = Duration::from_millis(350);
    const CHECK_QUIET: Duration = FRAME;
    const MARK_GUARD: Duration = Duration::from_millis(5);
    const MIN_QUIET_DBFS: f64 = -37.5;

    let fixture = Fixture::with_recorder_quantum();
    let doap = fixture.device.pods.doap();
    doap.hold_start(true);
    doap.burst(BURST, BURST);
    doap.silent(LEADING);
    doap.quiet(QUIET);
    doap.mark(MARK_FRAMES);
    fixture.list(CAPTURE);
    let recorder = fixture.capture();
    let mode = mode_set_at(&fixture);
    fixture.start(1);
    pause(RELEASE.saturating_sub(mode.elapsed()));
    doap.release_start();
    // The first steady frame places the opening at 240 ms, so the driver stops while the quiet onset is still near the reader.
    pause(PAUSE_AT.saturating_sub(mode.elapsed()));
    fixture.server.while_stopped(|| pause(STALL));
    pause(PLAY_WORD);

    let samples = recorder.finish();
    fixture.stopped(1);
    fixture.no_uplink();
    let marks = dictation_marks(&samples);
    let mark = marks
        .iter()
        .max_by_key(|mark| mark.len())
        .expect("no opening word recorded")
        .start;
    let quiet_end = mark.checked_sub(frames(MARK_GUARD)).unwrap();
    let quiet_start = quiet_end.checked_sub(frames(CHECK_QUIET)).unwrap();
    let quiet = &samples[quiet_start..quiet_end];
    let level = rms_dbfs(
        quiet
            .iter()
            .map(|&sample| f64::from(sample) / f64::from(i16::MAX)),
    );
    writeln!(
        io::stderr(),
        "quiet opening at {level:.1} dBFS, marks {marks:?}"
    )
    .unwrap();
    assert!(
        level >= MIN_QUIET_DBFS,
        "stalled reader lost the quiet opening word ({level:.1} dBFS)"
    );
    assert_marks(&samples, 1, MARK_LENGTH, (0, None));
    fixture.finish();
}

#[test]
fn late_anchor_keeps_the_last_word_at_the_tail_deadline() {
    const BURST: u32 = 4;
    const PER_NOTIFICATION: u32 = 3;
    const LEADING: u32 = 13;
    const FIRST_WORD: u32 = 2;
    const RELEASE: Duration = Duration::from_millis(317);
    const STALL: Duration = Duration::from_millis(67);
    const MARK_AT: Duration = Duration::from_millis(1_510);
    const PLAY_WORD: Duration = Duration::from_millis(240);
    const TAIL_DEADLINE: Duration = Duration::from_millis(1_277);

    let fixture = Fixture::with_recorder_quantum();
    let doap = fixture.device.pods.doap();
    doap.hold_start(true);
    doap.burst(BURST, PER_NOTIFICATION);
    doap.silent(LEADING);
    doap.mark(FIRST_WORD);
    // The 3+1 burst sets the initial anchor late. Pausing live packets gives the reader time to use it.
    doap.stall(STALL);
    fixture.list(CAPTURE);
    let recorder = fixture.capture();
    let mode = mode_set_at(&fixture);
    fixture.start(1);
    pause(RELEASE.saturating_sub(mode.elapsed()));
    doap.release_start();
    pause(MARK_AT.saturating_sub(mode.elapsed()));
    doap.mark(MARK_FRAMES);
    pause(PLAY_WORD);
    let samples = recorder.finish();
    fixture.stopped(1);
    fixture.no_uplink();
    let marks = dictation_marks(&samples);
    assert_eq!(marks.len(), 2, "{marks:?}");
    assert_marks(&samples[marks[0].end..], 1, MARK_LENGTH, (0, None));
    // The words span 1,300 ms in the input, or less on the recorder clock once the reader catches up.
    let tail = span_of(marks[1].end.checked_sub(marks[0].start).unwrap());
    writeln!(io::stderr(), "tail ends at {tail:?}, marks {marks:?}").unwrap();
    assert!(
        tail <= TAIL_DEADLINE,
        "late anchor lost the last word at {TAIL_DEADLINE:?}, it ended at {tail:?}, {marks:?}"
    );
    fixture.finish();
}
