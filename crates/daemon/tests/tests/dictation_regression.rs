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

/// Sleeps until `at` after `mode`, and fails when the test thread got there too late for the case to run as written.
fn pause_until(mode: Instant, at: Duration) {
    const SCHEDULE_SLACK: Duration = Duration::from_millis(5);
    pause(at.saturating_sub(mode.elapsed()));
    let late = mode.elapsed().saturating_sub(at);
    assert!(
        late <= SCHEDULE_SLACK,
        "test thread reached its {at:?} step {late:?} late, so the case did not run"
    );
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
    fixture.server.while_driver_paused(
        || {
            pause_until(mode, RELEASE);
            doap.release_start();
            // The first steady frame places the opening at 240 ms, before the graph stalls at the quiet onset.
            pause_until(mode, PAUSE_AT);
        },
        || {
            let paused_at = mode.elapsed();
            assert!(
                paused_at <= PAUSE_AT + FRAME,
                "PipeWire pause finished at {paused_at:?}, after the quiet onset could reach the reader"
            );
            // Allow the graph's last in-flight cycle to reach the recorder before checking the stall.
            pause(FRAME);
            let frames = recorder.frames_on_disk();
            pause(STALL.saturating_sub(FRAME));
            assert_eq!(
                recorder.frames_on_disk(),
                frames,
                "PipeWire driver did not pause"
            );
        },
    );
    let resumed_from = recorder.frames_on_disk();
    pause(PLAY_WORD);
    // pw-cli exits 0 on a driver command it rejects, so the resume is checked on the recorder.
    assert!(
        recorder.frames_on_disk() > resumed_from,
        "PipeWire driver did not resume"
    );

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
    const BURST: u32 = 3;
    const PER_NOTIFICATION: u32 = 2;
    const LEADING: u32 = 13;
    const FIRST_WORD: u32 = 2;
    const RELEASE: Duration = Duration::from_millis(317);
    const STALL: Duration = Duration::from_millis(67);
    const MARK_AT: Duration = Duration::from_millis(1_510);
    const PLAY_WORD: Duration = Duration::from_millis(240);
    const TAIL_DEADLINE: Duration = Duration::from_millis(1_277);
    // Ruling A closes at most 43 ms in 1 s at 1.045, so the 1,300 ms input keeps 1,252 ms after 5 ms of analysis slack.
    const TAIL_FLOOR: Duration = Duration::from_millis(1_252);

    let mut fixture = Fixture::with_recorder_quantum();
    let doap = fixture.device.pods.doap();
    doap.hold_start(true);
    doap.burst(BURST, PER_NOTIFICATION);
    doap.silent(LEADING);
    doap.mark(FIRST_WORD);
    // The 2+1 burst sets the initial anchor late and fits inside the reader's normal latency.
    doap.stall(STALL);
    fixture.list(CAPTURE);
    let recorder = fixture.capture();
    let mode = mode_set_at(&fixture);
    fixture.start(1);
    pause_until(mode, RELEASE);
    doap.release_start();
    pause_until(mode, MARK_AT);
    doap.mark(MARK_FRAMES);
    pause(PLAY_WORD);
    let samples = recorder.finish();
    fixture.stopped(1);
    fixture.no_uplink();
    assert_eq!(opening(&mut fixture), (0, None), "opening catch-up ran");
    let marks = dictation_marks(&samples);
    assert_eq!(marks.len(), 2, "{marks:?}");
    assert_marks(
        &samples[..marks[0].end],
        1,
        FRAME.saturating_mul(FIRST_WORD),
        (0, None),
    );
    assert_marks(&samples[marks[0].end..], 1, MARK_LENGTH, (0, None));
    // The words span 1,300 ms in the input, or less on the recorder clock once the reader catches up.
    let tail = span_of(marks[1].end.checked_sub(marks[0].start).unwrap());
    writeln!(io::stderr(), "tail ends at {tail:?}, marks {marks:?}").unwrap();
    assert!(
        tail <= TAIL_DEADLINE,
        "late anchor lost the last word at {TAIL_DEADLINE:?}, it ended at {tail:?}, {marks:?}"
    );
    assert!(
        tail >= TAIL_FLOOR,
        "catch-up dropped audio between the words, the tail ended at {tail:?}, {marks:?}"
    );
    fixture.finish();
}
