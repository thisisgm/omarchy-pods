//! The microphone uplink through the built daemon against the fake pods: when a START goes out, what the pods' markers move, the confirmed models in `settings.json`, and `off`.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use pods_core::policy::mic::{
        FAILED_LINE, NO_UPLINK_LINE, START_ANSWER_MS, START_TRIES, STOP_OWED_LINE,
        STOP_UNCONFIRMED_LINE, unanswered_line,
    };
    use pods_core::prefs::{MAX_CONFIRMED_MODELS, PrefsError};
    use pods_fake::Profile;
    use pods_samples::{MODEL_NUMBER, Placed};
    use serde_json::json;

    use super::support::rig::assert_gap;
    use super::support::settings::{
        UNREADABLE, plant_settings, set, status, stop, unreadable_lines,
    };
    use super::support::uplink::{
        START_ANSWER, STOP_ANSWER, all_tries, kept_models, link, lose, mic, put_in, read_up_to_now,
        rig_with, starts, stops, take_out, wait_kept_models, wait_mic, wait_starts, wait_stop,
    };
    use super::support::{Session, wait_for};

    /// Well inside `START_ANSWER_MS` after the START, so a held START is still unanswered at its end.
    const STILL_STARTING: Duration = Duration::from_millis(START_ANSWER_MS / 3);

    fn in_the_case() -> Profile {
        Profile {
            primary: Placed::InCase,
            secondary: Placed::InCase,
            ..Profile::default()
        }
    }

    #[test]
    fn always_and_a_bud_worn_starts_the_uplink_once() {
        let (rig, device) = rig_with("always", &[], Profile::default());
        link(&rig, &device);
        wait_mic(&rig, "live");
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        assert_eq!(mic(&rig), "live");
        assert!(device.pods.uplink().running());
        rig.finish();
    }

    #[test]
    fn no_bud_worn_sends_no_start() {
        let (rig, device) = rig_with("always", &[], in_the_case());
        link(&rig, &device);
        assert!(starts(&device).is_empty(), "{:?}", device.pods.heard());
        assert_eq!(mic(&rig), "not_worn");
        device.pods.set_ear(Placed::InEar, Placed::InCase);
        wait_starts(&device, 1);
        wait_mic(&rig, "live");
        rig.finish();
    }

    #[test]
    fn the_preference_off_sends_no_start_and_reads_off() {
        let (rig, device) = rig_with("off", &[], Profile::default());
        link(&rig, &device);
        assert!(starts(&device).is_empty(), "{:?}", device.pods.heard());
        assert_eq!(mic(&rig), "off");
        rig.finish();
    }

    #[test]
    fn an_unconfirmed_model_is_started_once_and_stored() {
        let (rig, device) = rig_with("auto", &[], Profile::default());
        link(&rig, &device);
        wait_starts(&device, 1);
        wait_mic(&rig, "live");
        wait_kept_models(rig.session(), &[MODEL_NUMBER]);
        lose(&rig, &device);
        link(&rig, &device);
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        wait_mic(&rig, "no_audio_server");
        rig.finish();
    }

    #[test]
    fn the_running_marker_not_the_start_moves_the_state() {
        let (rig, device) = rig_with("always", &[], Profile::default());
        device.pods.uplink().hold_starts(true);
        link(&rig, &device);
        let start = wait_starts(&device, 1)[0].at;
        let until = start + STILL_STARTING;
        rig.holds_for(
            until.saturating_duration_since(Instant::now()),
            "mic starting",
            |status| status["mic"] == "starting",
        );
        device.pods.uplink().release_start();
        wait_mic(&rig, "live");
        rig.finish();
    }

    #[test]
    fn auto_with_a_confirmed_model_sends_nothing() {
        let (mut rig, device) = rig_with("auto", &[MODEL_NUMBER], Profile::default());
        link(&rig, &device);
        assert!(starts(&device).is_empty(), "{:?}", device.pods.heard());
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        wait_mic(&rig, "no_audio_server");
        let log = rig.daemon().log();
        let uplink = |line: &str| line.starts_with("mic: ") && !line.starts_with("mic: source");
        assert!(!log.lines().any(uplink), "{log}");
        rig.finish();
    }

    #[test]
    fn setting_off_with_no_bud_worn_sends_stop_at_once() {
        let (rig, device) = rig_with("always", &[], Profile::default());
        link(&rig, &device);
        wait_mic(&rig, "live");
        take_out(&rig, &device);
        let written = Instant::now();
        set(rig.session(), "microphone", "off");
        let stop = wait_stop(&device);
        // Core's `off_and_the_exit_owe_the_stop_with_no_linger_and_only_through_the_gate` owns "at once": the STOP leaves in the write's own step.
        assert_gap(
            "the STOP after the write",
            written,
            stop.at,
            &(Duration::ZERO..),
        );
        wait_for("the uplink to stop", || {
            (!device.pods.uplink().running()).then_some(())
        });
        wait_mic(&rig, "off");
        assert_eq!(stops(&device).len(), 1);
        rig.finish();
    }

    #[test]
    fn setting_off_while_starting_sends_no_stop_until_the_start_is_answered() {
        let (mut rig, device) = rig_with("always", &[], Profile::default());
        device.pods.uplink().hold_starts(true);
        link(&rig, &device);
        wait_starts(&device, 1);
        set(rig.session(), "microphone", "off");
        assert_eq!(mic(&rig), "stopping");
        rig.wait_logged(STOP_OWED_LINE);
        read_up_to_now(&rig, &device);
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        // No bud worn, so the gate holds the owed STOP no longer than the START does.
        take_out(&rig, &device);
        let released = Instant::now();
        device.pods.uplink().release_start();
        let stop = wait_stop(&device);
        // Core's `off_and_the_exit_owe_the_stop_with_no_linger_and_only_through_the_gate` owns "at once": the STOP leaves in the answer's own step.
        assert_gap(
            "the STOP after the answer",
            released,
            stop.at,
            &(Duration::ZERO..),
        );
        let log = rig.daemon().log();
        assert!(!log.lines().any(|line| line == unanswered_line(1)), "{log}");
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        wait_mic(&rig, "off");
        rig.finish();
    }

    #[test]
    fn setting_off_while_starting_and_never_answered_sends_nothing() {
        let (mut rig, device) = rig_with("always", &[], Profile::default());
        device.pods.uplink().never_answer(true);
        link(&rig, &device);
        wait_starts(&device, 1);
        set(rig.session(), "microphone", "off");
        assert_eq!(mic(&rig), "stopping");
        rig.wait_logged(&unanswered_line(1));
        read_up_to_now(&rig, &device);
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        assert_eq!(mic(&rig), "off");
        rig.finish();
    }

    #[test]
    fn a_late_answer_after_off_is_stopped_at_once() {
        let (mut rig, device) = rig_with("always", &[], in_the_case());
        device.pods.uplink().hold_starts(true);
        link(&rig, &device);
        let worn = Instant::now();
        device.pods.set_ear(Placed::InEar, Placed::InCase);
        wait_starts(&device, 1);
        set(rig.session(), "microphone", "off");
        rig.wait_logged(&unanswered_line(1));
        // The upper bound is core's `each_answer_window_ends_at_its_deadline_and_not_before`, on its pure clock.
        assert_gap(
            "the unanswered line after a bud went in",
            worn,
            Instant::now(),
            &(START_ANSWER..),
        );
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        // No bud worn, so the gate lets the late answer's STOP out.
        take_out(&rig, &device);
        let released = Instant::now();
        device.pods.uplink().release_start();
        let stop = wait_stop(&device);
        // Core's `off_and_the_exit_owe_the_stop_with_no_linger_and_only_through_the_gate` owns "at once": the STOP leaves in the answer's own step.
        assert_gap(
            "the STOP after the late answer",
            released,
            stop.at,
            &(Duration::ZERO..),
        );
        wait_mic(&rig, "off");
        rig.finish();
    }

    #[test]
    fn a_stop_never_answered_is_logged_and_ends_stopping() {
        let (mut rig, device) = rig_with("always", &[], Profile::default());
        device.pods.uplink().hold_stops(true);
        link(&rig, &device);
        wait_mic(&rig, "live");
        take_out(&rig, &device);
        let written = Instant::now();
        set(rig.session(), "microphone", "off");
        wait_stop(&device);
        rig.wait_logged(STOP_UNCONFIRMED_LINE);
        // The upper bound is core's `each_answer_window_ends_at_its_deadline_and_not_before`, on its pure clock.
        assert_gap(
            "the unconfirmed line after the write",
            written,
            Instant::now(),
            &(STOP_ANSWER..),
        );
        wait_mic(&rig, "off");
        set(rig.session(), "microphone", "always");
        put_in(&rig, &device);
        wait_starts(&device, 2);
        device.pods.uplink().release_stop();
        read_up_to_now(&rig, &device);
        assert_eq!(stops(&device).len(), 1, "{:?}", device.pods.heard());
        assert_eq!(mic(&rig), "starting");
        rig.finish();
    }

    #[test]
    fn always_sends_no_start_without_a_model_number() {
        let unnumbered = Profile {
            model_number: String::new(),
            ..Profile::default()
        };
        let (rig, device) = rig_with("always", &[], unnumbered);
        link(&rig, &device);
        let status = rig.status();
        assert_eq!(status["connected"], true, "{status}");
        assert_eq!(status["left"]["in_ear"], true, "{status}");
        assert!(starts(&device).is_empty(), "{:?}", device.pods.heard());
        assert_eq!(mic(&rig), "idle");
        rig.finish();
    }

    #[test]
    fn a_link_lost_while_live_sends_nothing() {
        let (rig, device) = rig_with("always", &[], Profile::default());
        link(&rig, &device);
        wait_mic(&rig, "live");
        lose(&rig, &device);
        assert_eq!(mic(&rig), "idle");
        device.pods.uplink().hold_starts(true);
        link(&rig, &device);
        wait_starts(&device, 2);
        wait_mic(&rig, "starting");
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        rig.finish();
    }

    #[test]
    fn a_model_that_never_answers_is_unsupported_and_not_stored() {
        let (mut rig, device) = rig_with("auto", &[], in_the_case());
        device.pods.uplink().never_answer(true);
        link(&rig, &device);
        let worn = Instant::now();
        device.pods.set_ear(Placed::InEar, Placed::InCase);
        wait_starts(&device, 1);
        wait_mic(&rig, "unsupported");
        // The upper bound is core's `an_unanswered_start_is_sent_three_times_and_each_try_is_counted`, on its pure clock.
        assert_gap(
            "unsupported after a bud went in",
            worn,
            Instant::now(),
            &(START_ANSWER.saturating_mul(START_TRIES)..),
        );
        rig.wait_logged(NO_UPLINK_LINE);
        read_up_to_now(&rig, &device);
        assert_eq!(
            starts(&device).len(),
            all_tries(),
            "{:?}",
            device.pods.heard()
        );
        assert_eq!(kept_models(rig.session()), json!([]));
        lose(&rig, &device);
        link(&rig, &device);
        wait_starts(&device, all_tries().saturating_add(1));
        wait_mic(&rig, "starting");
        rig.finish();
    }

    #[test]
    fn a_confirmed_model_that_never_answers_reads_failed() {
        let (mut rig, device) = rig_with("always", &[MODEL_NUMBER], Profile::default());
        device.pods.uplink().never_answer(true);
        link(&rig, &device);
        wait_starts(&device, 1);
        wait_mic(&rig, "failed");
        rig.wait_logged(FAILED_LINE);
        read_up_to_now(&rig, &device);
        assert_eq!(
            starts(&device).len(),
            all_tries(),
            "{:?}",
            device.pods.heard()
        );
        assert_eq!(kept_models(rig.session()), json!([MODEL_NUMBER]));
        rig.finish();
    }

    #[test]
    fn a_settings_file_with_a_bad_model_entry_is_refused() {
        let seventeen: Vec<String> = (0..=MAX_CONFIRMED_MODELS)
            .map(|number| format!("B{number:04}"))
            .collect();
        for models in [
            json!(["a3063"]),
            json!([""]),
            json!(["A30630000"]),
            json!(["A30-63"]),
            json!(["A3063", "A3063"]),
            json!([3063]),
            json!("A3063"),
            json!(seventeen),
        ] {
            let text = json!({
                "version": 1,
                "ear_detection": "both",
                "notifications": false,
                "microphone": "always",
                "mic_models": models,
            });
            let line = format!("{UNREADABLE}: {}", PrefsError::MicModels);
            let session = Session::new();
            plant_settings(&session, text.to_string().as_bytes());
            let mut daemon = session.start();
            let read = status(&session);
            assert_eq!(read["microphone"], "auto", "{text}: {read}");
            assert_eq!(read["notifications_enabled"], true, "{text}: {read}");
            let log = daemon.log();
            assert_eq!(unreadable_lines(&daemon), 1, "{text}: {log}");
            assert!(log.lines().any(|logged| logged == line), "{text}: {log}");
            stop(&mut daemon);
        }
    }

    #[test]
    fn a_seventeenth_model_drops_the_oldest() {
        let planted: Vec<String> = (1..=MAX_CONFIRMED_MODELS)
            .map(|number| format!("B{number:04}"))
            .collect();
        let planted: Vec<&str> = planted.iter().map(String::as_str).collect();
        let (rig, device) = rig_with("auto", &planted, Profile::default());
        link(&rig, &device);
        wait_starts(&device, 1);
        let mut wanted = planted[1..].to_vec();
        wanted.push(MODEL_NUMBER);
        wait_kept_models(rig.session(), &wanted);
        rig.finish();
    }
}
