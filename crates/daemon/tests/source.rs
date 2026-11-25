//! The microphone's source node through the built daemon against the fake pods, the fake audio server and a private `PipeWire`: its name and latency, the recorder that starts the uplink, and every way the node goes.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pods_core::policy::mic::{
        SOURCE_UP_LINE, STOP_QUIET_LINE, STOPPED_BY_THE_PODS_LINE, withdrawn_line,
    };
    use pods_fake::Profile;
    use pods_mic::{NODE_LATENCY_MS, RATE_HZ};
    use pods_samples::{MODEL_NUMBER, NAME, Placed};
    use pulseaudio::protocol::SinkState;
    use serde_json::{Value, json};

    use super::support::pipewire::{
        daemon_states_latency, links_of, node_name, ready, record, start, wait_no_node, wait_node,
    };
    use super::support::rig::REAL_TIME_SLACK;
    use super::support::settings::set;
    use super::support::uplink::{
        assert_no_sink_call, link, link_heard, lose, mic, move_sink, read_up_to_now, rig_with,
        rig_with_audio, starts, stops, wait_mic, wait_restore, wait_starts, wait_stop,
    };
    use super::support::{HANG_LIMIT, pause, wait_for};

    const NANOS_PER_MILLI: u64 = 1_000_000;
    /// How the daemon's lines and the node may write the fixture address.
    const ADDRESS_FORMS: [&str; 2] = ["02:00:00", "02_00_00"];
    /// A recording long enough to hold frames: a tenth of a second at Superwhisper's rate.
    const SOME_FRAMES: usize = 1600;
    /// The node's own description of the sample pods' model.
    const DESCRIPTION: &str = "AirPods Pro 3 Microphone";
    /// The first step of `AUDIO_RETRY_MS` in `src/daemon/audio.rs`, tied by a test beside the adapter.
    const FIRST_RETRY: Duration = Duration::from_millis(250);

    fn latency_ns() -> u64 {
        u64::from(NODE_LATENCY_MS).saturating_mul(NANOS_PER_MILLI)
    }

    /// The position of the first line of `log` that `matches`, failing with the log when none does.
    fn position(log: &str, what: &str, matches: impl Fn(&str) -> bool) -> usize {
        log.lines()
            .position(matches)
            .unwrap_or_else(|| panic!("no line {what}: {log}"))
    }

    #[test]
    fn a_worn_bud_brings_the_node_up_with_its_name_and_latency() {
        let (mut rig, _device, pipewire) = ready();
        let node = wait_node(&pipewire);
        let props = &node["info"]["props"];
        let expected = json!({
            "media.class": "Audio/Source",
            "node.name": node_name(),
            "node.description": DESCRIPTION,
            "priority.session": 3100,
        });
        for (key, value) in expected.as_object().unwrap() {
            assert_eq!(props.get(key), Some(value), "{key} in {props}");
        }
        let format = &node["info"]["params"]["EnumFormat"][0];
        assert_eq!(format["rate"], RATE_HZ, "{format}");
        assert_eq!(format["channels"], 1, "{format}");
        assert!(
            ["F32LE", "F32"].contains(&format["format"].as_str().unwrap_or_default()),
            "{format}"
        );
        // An older library keeps the parameter to itself and shows an empty list, measured on 1.0.5.
        let (stated, times) = if daemon_states_latency(rig.daemon().pid()) {
            (json!([{"quantum": 0.0, "rate": 0, "ns": latency_ns()}]), 1)
        } else {
            (json!([]), 0)
        };
        assert_eq!(node.pointer("/info/params/ProcessLatency"), Some(&stated));
        // Stated once: no latency of the node or of a port repeats the figure.
        let dump = pipewire.dump().to_string();
        let figure = latency_ns().to_string();
        assert_eq!(dump.matches(&figure).count(), times, "{dump}");
        assert_eq!(rig.status()["mic_source"], node_name());
        rig.wait_logged(SOURCE_UP_LINE);
        rig.finish();
    }

    #[test]
    fn no_property_of_the_node_holds_an_address_or_the_device_name() {
        let (rig, _device, pipewire) = ready();
        let node = wait_node(&pipewire);
        let props = node["info"]["props"].as_object().unwrap();
        for (key, value) in props {
            let text = value.to_string().to_lowercase();
            assert!(!text.contains(&NAME.to_lowercase()), "{key} is {text}");
            for form in ADDRESS_FORMS {
                assert!(!text.contains(form), "{key} is {text}");
            }
        }
        let dump = pipewire.dump().to_string().to_lowercase();
        assert!(!dump.contains(&NAME.to_lowercase()), "{dump}");
        for form in ADDRESS_FORMS {
            assert!(!dump.contains(form), "{dump}");
        }
        rig.finish();
    }

    #[test]
    fn the_first_recorder_starts_the_uplink_cold() {
        let (rig, device, pipewire) = ready();
        read_up_to_now(&rig, &device);
        assert!(starts(&device).is_empty(), "{:?}", device.pods.heard());
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_starts(&device, 1);
        wait_mic(&rig, "recording");
        recorder.wait_frames(SOME_FRAMES, HANG_LIMIT);
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        drop(recorder.finish());
        rig.finish();
    }

    #[test]
    fn two_recorders_inside_the_linger_send_one_start_and_no_stop() {
        let (rig, device, pipewire) = ready();
        let first = record(&rig, &pipewire, "first.wav");
        wait_mic(&rig, "recording");
        drop(first.finish());
        wait_mic(&rig, "live");
        let second = record(&rig, &pipewire, "second.wav");
        wait_mic(&rig, "recording");
        drop(second.finish());
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        rig.finish();
    }

    #[test]
    fn the_node_has_no_link_and_does_not_run_with_no_recorder() {
        let (rig, device) = rig_with("always", &[MODEL_NUMBER], Profile::default());
        let pipewire = start(&rig);
        link(&rig, &device);
        wait_mic(&rig, "live");
        let node = wait_node(&pipewire);
        let dump = pipewire.dump();
        assert_eq!(links_of(&dump, &node), Vec::<Value>::new(), "{dump}");
        assert_ne!(node["info"]["state"], "running", "{node}");
        rig.finish();
    }

    #[test]
    fn the_preference_off_removes_the_node_and_stops_the_uplink() {
        let (mut rig, device, audio) = rig_with_audio("auto", &[MODEL_NUMBER], SinkState::Running);
        let pipewire = start(&rig);
        link_heard(&rig, &device);
        wait_node(&pipewire);
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        set(rig.session(), "microphone", "off");
        assert_eq!(mic(&rig), "stopping");
        wait_no_node(&pipewire);
        // The node is gone at the write; the STOP waits for the playing sink of a worn bud.
        move_sink(&rig, &audio, &device, SinkState::Suspended);
        wait_stop(&device);
        wait_for("the uplink to stop", || {
            (!device.pods.uplink().running()).then_some(())
        });
        drop(recorder.finish());
        let daemon = rig.daemon();
        // The stopped marker's line is the last of the three, so the log holds them all once it shows.
        let log = wait_for("the stopped marker's line", || {
            let log = daemon.log();
            log.lines()
                .any(|line| line.starts_with("mic: uplink stopped after "))
                .then_some(log)
        });
        let withdrawn = position(&log, "of the withdrawal", |line| {
            line == withdrawn_line("off")
        });
        let stop = position(&log, "of the STOP", |line| line == STOP_QUIET_LINE);
        let stopped = position(&log, "of the stopped marker", |line| {
            line.starts_with("mic: uplink stopped after ")
        });
        assert!(withdrawn < stop && stop < stopped, "{log}");
        assert_eq!(stops(&device).len(), 1, "{:?}", device.pods.heard());
        assert_no_sink_call(&rig, &audio);
        rig.finish();
    }

    #[test]
    fn a_recorder_kept_through_a_stop_by_the_pods_starts_again_only_when_it_links_again() {
        let (mut rig, device, audio) = rig_with_audio("auto", &[MODEL_NUMBER], SinkState::Running);
        let pipewire = start(&rig);
        link_heard(&rig, &device);
        wait_node(&pipewire);
        let first = record(&rig, &pipewire, "first.wav");
        wait_mic(&rig, "recording");
        device.pods.uplink().stop_unasked();
        rig.wait_logged(STOPPED_BY_THE_PODS_LINE);
        wait_restore(&audio, &device);
        // A second START shows `starting`, never `ready`, while the recorder stays linked.
        wait_mic(&rig, "ready");
        let dump = pipewire.dump();
        let node = wait_node(&pipewire);
        assert_ne!(links_of(&dump, &node), Vec::<Value>::new(), "{dump}");
        assert_eq!(node["info"]["state"], "running", "{node}");
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        drop(first.finish());
        wait_for("the node idle once the recorder left", || {
            (wait_node(&pipewire)["info"]["state"] != "running").then_some(())
        });
        // A START for the leave alone shows `starting` or `live` inside the window.
        rig.holds_for(
            REAL_TIME_SLACK,
            "mic ready once the recorder left",
            |status| status["mic"] == "ready",
        );
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        let second = record(&rig, &pipewire, "second.wav");
        wait_starts(&device, 2);
        wait_mic(&rig, "recording");
        drop(second.finish());
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 2, "{:?}", device.pods.heard());
        rig.finish();
    }

    #[test]
    fn the_link_lost_removes_the_node_and_ends_the_recorder_cleanly() {
        let (mut rig, device, pipewire) = ready();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        lose(&rig, &device);
        wait_no_node(&pipewire);
        rig.wait_logged(&withdrawn_line("link"));
        drop(recorder.finish());
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        rig.finish();
    }

    #[test]
    fn with_no_pipewire_the_code_is_no_audio_server_and_the_link_is_kept() {
        let (rig, device) = rig_with("auto", &[MODEL_NUMBER], Profile::default());
        link(&rig, &device);
        wait_mic(&rig, "no_audio_server");
        read_up_to_now(&rig, &device);
        let status = rig.status();
        assert_eq!(status["connected"], true, "{status}");
        assert_eq!(status["mic"], "no_audio_server", "{status}");
        assert_eq!(status["mic_source"], "", "{status}");
        rig.finish();
    }

    #[test]
    fn an_outage_logs_its_reason_once_and_a_late_server_gets_the_node() {
        let (mut rig, device) = rig_with("auto", &[MODEL_NUMBER], Profile::default());
        link(&rig, &device);
        wait_mic(&rig, "no_audio_server");
        // Past the first retry, so the absent server has been missed at least twice.
        pause(FIRST_RETRY + REAL_TIME_SLACK);
        let pipewire = start(&rig);
        wait_node(&pipewire);
        wait_mic(&rig, "ready");
        let log = rig.daemon().log();
        let reasons = log
            .lines()
            .filter(|line| line.starts_with("mic: source could not open: no audio server: "));
        assert_eq!(reasons.count(), 1, "one line an outage: {log}");
        rig.finish();
    }

    #[test]
    fn exit_removes_the_node() {
        let (mut rig, _device, pipewire) = ready();
        rig.term();
        let status = rig.exited(HANG_LIMIT);
        assert!(status.success(), "{status}");
        wait_no_node(&pipewire);
        rig.wait_logged(&withdrawn_line("exit"));
        rig.finish();
    }

    #[test]
    fn the_log_holds_no_address_and_no_name() {
        let (mut rig, device, pipewire) = ready();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        drop(recorder.finish());
        device.pods.set_ear(Placed::InCase, Placed::InCase);
        rig.wait_logged(&withdrawn_line("not_worn"));
        device.pods.set_ear(Placed::InEar, Placed::InEar);
        wait_mic(&rig, "ready");
        set(rig.session(), "microphone", "off");
        rig.wait_logged(&withdrawn_line("off"));
        set(rig.session(), "microphone", "auto");
        wait_mic(&rig, "ready");
        pipewire.stop();
        rig.wait_logged(&withdrawn_line("server"));
        lose(&rig, &device);
        rig.term();
        assert!(rig.exited(HANG_LIMIT).success());
        let log = rig.daemon().log().to_lowercase();
        assert!(!log.contains(&NAME.to_lowercase()), "{log}");
        for form in ADDRESS_FORMS {
            assert!(!log.contains(form), "{log}");
        }
        rig.finish();
    }
}
