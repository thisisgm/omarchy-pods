//! What a dictation keeps of its opening: a burst flushed at stop, and a reconnected recorder after a stray uplink SDU.

use super::*;

#[test]
fn a_reconnected_recorder_keeps_its_opening_after_a_stray_uplink_sdu() {
    use pods_fake::doap::Table;
    use pods_fake::uplink::{DEFAULT_LEVEL, TONE_LINE, UNITS_PER_PACKET};
    use pods_samples::MODEL_NUMBER;
    use pods_samples::uplink::{audio_sdu, tone_unit};
    use pods_trap::Answer;

    use super::super::support::doap::route;
    use super::super::support::pipewire::{links_of, node_name, start, wait_node};
    use super::super::support::recording::LEFT;
    use super::super::support::uplink::{link, lose, rig_with_audio};

    const ARM_DELAY: Duration = Duration::from_millis(750);
    const FIRST_SPAN: Duration = Duration::from_millis(57);
    const HOLD_OPENING: Duration = Duration::from_millis(240);
    const LINKED: &str = "mic: recorder linked";

    let (mut rig, device, audio) = rig_with_audio("auto", &[MODEL_NUMBER], SinkState::Running);
    let source = node_name();
    audio.add_source(&source);
    let server = start(&rig);
    let doap = device.pods.doap();
    doap.table(Table::Observed);
    device.pods.uplink().hold_starts(true);
    link(&rig, &device);
    status(&rig, "armed");
    lose(&rig, &device);
    status(&rig, "off");

    let mut att = route(&device);
    let Answer::Accept { after, .. } = &mut att.answer else {
        panic!("expected the fake pods to accept the attribute channel");
    };
    *after = ARM_DELAY;
    rig.route(att);
    audio.add_client(Some("true"));
    link(&rig, &device);
    rig.wait_sink_told();
    status(&rig, "arming");
    status(&rig, "armed");
    let node = wait_node(&server);
    let log_path = rig.daemon().log.clone();
    let log = || fs::read_to_string(&log_path).unwrap();

    doap.hold_start(true);
    let recorder = record(&rig, &server, "reconnect.wav");
    rig.wait_logged(LINKED);
    let linked = Instant::now();
    let links = links_of(&server.dump(), &node);
    assert_eq!(links.len(), 1);
    let link_id = links[0]["id"].as_u64().unwrap().to_string();
    pause(FIRST_SPAN.saturating_sub(linked.elapsed()));
    let unlinked = server
        .command("pw-link")
        .args(["-d", &link_id])
        .output()
        .unwrap();
    assert!(unlinked.status.success(), "{unlinked:?}");
    wait_for("the first span to end", || {
        log()
            .lines()
            .any(|line| line.starts_with(LEFT))
            .then_some(())
    });
    server.link(&source, recorder.node());
    wait_for("the second span to start", || {
        (log().lines().filter(|line| *line == LINKED).count() == 2).then_some(())
    });
    let unit = tone_unit(TONE_LINE, DEFAULT_LEVEL);
    let units = vec![unit; usize::try_from(UNITS_PER_PACKET).unwrap()];
    device.pods.send(&audio_sdu(0, &units));
    read_up_to_now(&rig, &device);

    audio.add_recorder(&source, CAPTURE);
    wait_for("dictation mode set", || doap.mode_seen().then_some(()));
    let marked = Instant::now();
    doap.mark(MARK_FRAMES);
    wait_for("DoAP START", || (doap.starts() == 1).then_some(()));
    pause(HOLD_OPENING.saturating_sub(marked.elapsed()));
    doap.release_start();
    status(&rig, "streaming");
    ready_to_read(&recorder, HOLD_OPENING + MAX_TAIL);
    pause(HOLD_OPENING + MAX_TAIL);
    let samples = recorder.finish();
    status(&rig, "armed");

    let mut fixture = Fixture {
        rig,
        device,
        audio,
        server,
    };
    let timing = opening(&mut fixture);
    let log = log();
    let second_span = log.lines().filter(|line| line.starts_with(LEFT)).nth(1);
    assert!(
        second_span.is_some_and(|line| line.contains("accepted SDUs 1,")),
        "{second_span:?}"
    );
    assert_marks(&samples, 1, MARK_LENGTH, timing);
    read_up_to_now(&fixture.rig, &fixture.device);
    fixture.no_uplink();
    fixture.finish();
}

#[test]
fn burst_only_short_dictation_plays_the_burst_when_flushed_at_stop() {
    use super::super::support::sleep::sleep;

    const BURST_FRAMES: u32 = 4;
    const BURST_LENGTH: Duration = FRAME.saturating_mul(BURST_FRAMES);
    const NO_STEADY: Duration = Duration::from_secs(2);
    const FLUSH_SPAN: Duration = BURST_LENGTH.saturating_add(MAX_TAIL);
    let mut fixture = Fixture::new();
    let doap = fixture.device.pods.doap();
    doap.burst(BURST_FRAMES, BURST_FRAMES);
    doap.mark(u32::MAX);
    doap.stall(NO_STEADY);
    fixture.list(CAPTURE);
    let recorder = record(&fixture.rig, &fixture.server, "burst.wav");
    fixture.streaming(1);
    wait_for("the burst notification", || {
        fixture
            .rig
            .daemon()
            .log()
            .contains("dictation: first audio after ")
            .then_some(())
    });
    sleep(&mut fixture.rig);
    fixture.stopped(1);
    pause(FLUSH_SPAN);
    assert_marks(&recorder.finish(), 1, BURST_LENGTH, (0, None));
    fixture.no_uplink();
    fixture.finish();
}

#[test]
fn two_dictations_keep_their_pre_roll_while_the_source_stays_running() {
    const PLAYBACK_SPAN: Duration = Duration::from_secs(30);
    const SILENT_FRAMES: u32 = 10;
    const HELD_START: Duration = FRAME.saturating_mul(SILENT_FRAMES + 1);
    const PLAY_OPENING: Duration = FRAME.saturating_mul(25);
    let fixture = Fixture::with_recorder_quantum();
    let playback = pace(&fixture.rig, &fixture.server, PLAYBACK_SPAN);
    let doap = fixture.device.pods.doap();
    for count in 1..=2 {
        doap.hold_start(true);
        doap.silent(SILENT_FRAMES);
        doap.mark(MARK_FRAMES);
        // With the source already running, listing the second recorder starts the DoAP deadline.
        let listed = (count == 1).then(|| fixture.list(CAPTURE));
        let recorder = fixture.capture();
        let listed = listed.unwrap_or_else(|| fixture.list(CAPTURE));
        wait_for("dictation mode set", || doap.mode_seen().then_some(()));
        let mode = Instant::now();
        if count == 1 {
            // The extra graph link keeps the source Streaming across the break in Pulse's list.
            fixture.server.link(&node_name(), playback.recorder.node());
        }
        fixture.start(count);
        pause(HELD_START.saturating_sub(mode.elapsed()));
        doap.release_start();
        fixture.streaming(count);
        pause(PLAY_OPENING);
        let samples = recorder.finish();
        assert_eq!(wait_node(&fixture.server)["info"]["state"], "running");
        fixture.audio.remove_recorder(listed);
        fixture.stopped(usize::try_from(count).unwrap());
        assert_marks(&samples, 1, MARK_LENGTH, (0, None));
    }
    drop(playback);
    fixture.no_uplink();
    fixture.finish();
}
