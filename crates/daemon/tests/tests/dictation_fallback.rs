//! Uplink audio after failed `DoAP` starts, through the daemon.

use super::*;

#[test]
fn dictation_records_uplink_after_two_unanswered_starts() {
    use pods_fake::uplink::PERIOD;

    use super::super::support::recording::marked;
    use super::super::support::uplink::starts;

    let mut fixture = Fixture::new();
    let doap = fixture.device.pods.doap();
    doap.hold_start(true);
    fixture.list(CAPTURE);
    let recorder = fixture.capture();
    uplink(&fixture);
    assert_eq!(doap.starts(), 2);
    wait_for("host STOP for both unanswered starts", || {
        (doap.stops() == [HOST_STOP; 2]).then_some(())
    });
    assert!(!doap.streaming());
    assert_eq!(fixture.mode_writes(), 2);
    wait_for("dictation mode cleared", || {
        (!doap.mode_seen()).then_some(())
    });

    fixture.device.pods.uplink().mark(MARK_FRAMES);
    pause(PERIOD.saturating_mul(MARK_FRAMES).saturating_add(MAX_TAIL));
    let samples = recorder.finish();
    fixture
        .rig
        .wait_logged("dictation: this recording takes the uplink");
    assert!(
        !marked(&samples).is_empty(),
        "missing fallback uplink audio"
    );
    assert_eq!(starts(&fixture.device).len(), 1);
    fixture.finish();
}

#[test]
fn the_first_uplink_start_after_two_refusals_records_audio() {
    use pods_fake::doap::MODE_SETTLE;
    use pods_fake::uplink::PERIOD;

    use super::super::support::recording::marked;
    use super::super::support::uplink::starts;

    let mut fixture = Fixture::new();
    fixture.device.pods.doap().refuse_starts(2);
    fixture.list(CAPTURE);
    let recorder = fixture.capture();
    uplink(&fixture);
    assert_eq!(fixture.device.pods.doap().starts(), 2);
    fixture.device.pods.uplink().mark(MARK_FRAMES);
    pause(PERIOD.saturating_mul(MARK_FRAMES).saturating_add(MAX_TAIL));
    let samples = recorder.finish();

    let starts = starts(&fixture.device);
    assert_eq!(starts.len(), 1, "the first uplink START must be answered");
    let cleared = fixture
        .device
        .pods
        .heard()
        .into_iter()
        .find(|heard| heard.sdu == doap_mode_off())
        .expect("fallback must clear the mode");
    assert!(starts[0].at.saturating_duration_since(cleared.at) >= MODE_SETTLE);
    let log = fixture.rig.daemon().log();
    assert!(log.lines().any(|line| line == "mic: start sent (try 1)"));
    assert!(
        !log.lines()
            .any(|line| line.starts_with("mic: start unanswered"))
    );
    assert!(
        !marked(&samples).is_empty(),
        "missing fallback uplink audio"
    );
    fixture.finish();
}
