//! The pods' audio through the built daemon into its node, against the fake pods and a private `PipeWire`: the first audio of a run, the units lost or refused and the counters they leave, by order and by count alone.

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

    use pods_core::policy::mic::{FIRST_AUDIO_PREFIX, RUNNING_AFTER_PREFIX};
    use pods_fake::uplink::UNITS_PER_PACKET;
    use pods_samples::{NAME, Placed};

    use super::support::HANG_LIMIT;
    use super::support::pipewire::{ready, record};
    use super::support::recording::{
        assert_sounds, counts, counts_after_leaving, first_sound, frames, wait_packets,
    };
    use super::support::settings::set;
    use super::support::uplink::{lose, read_up_to_now, wait_mic};

    /// How the daemon's lines may write the fixture address.
    const ADDRESS_FORMS: [&str; 2] = ["02:00:00", "02_00_00"];
    /// Enough of a recording that the uplink's audio has reached it.
    const SOME: Duration = Duration::from_millis(100);
    /// Enough of a later recording for the pods' audio to reach it again.
    const AGAIN: Duration = Duration::from_millis(300);
    /// Packets of garbled units for the decoder to refuse.
    const GARBLED: u32 = 10;
    const LEFT: &str = "mic: recorder left (underruns ";
    const LINKED: &str = "mic: recorder linked";

    /// The number of milliseconds in the first line of `log` that starts with `prefix`, and the line's place.
    // Sample input: `mic: first audio after 41 ms`.
    fn millis_after(log: &str, prefix: &str) -> (usize, u64) {
        let (place, line) = log
            .lines()
            .enumerate()
            .find(|(_, line)| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("no line `{prefix}`: {log}"));
        let millis = line
            .strip_prefix(prefix)
            .unwrap()
            .strip_suffix(" ms")
            .unwrap();
        (place, millis.parse().unwrap())
    }

    /// The underruns and concealed units of the recorder left line.
    // Sample input: `mic: recorder left (underruns 0, concealed 4)`.
    fn left_counts(line: &str) -> (u64, u64) {
        let inside = line.strip_prefix(LEFT).unwrap().strip_suffix(')').unwrap();
        let (underruns, concealed) = inside.split_once(", concealed ").unwrap();
        (underruns.parse().unwrap(), concealed.parse().unwrap())
    }

    #[test]
    fn a_cold_start_logs_the_marker_time_and_the_first_audio_time() {
        let (mut rig, _device, pipewire) = ready();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        let daemon = rig.daemon();
        let log = super::support::wait_for("the first audio's line", || {
            let log = daemon.log();
            log.contains(FIRST_AUDIO_PREFIX).then_some(log)
        });
        let (running_at, running_ms) = millis_after(&log, RUNNING_AFTER_PREFIX);
        let (audio_at, audio_ms) = millis_after(&log, FIRST_AUDIO_PREFIX);
        assert!(running_at < audio_at, "{log}");
        assert!(running_ms <= audio_ms, "{log}");
        drop(recorder.finish());
        // A second recorder on the same run, so a line logged again per recorder or per buffer shows.
        wait_mic(&rig, "live");
        let second = record(&rig, &pipewire, "second.wav");
        wait_mic(&rig, "recording");
        second.wait_frames(frames(AGAIN), HANG_LIMIT);
        first_sound(&second.finish());
        let log = rig.daemon().log();
        assert_eq!(log.matches(FIRST_AUDIO_PREFIX).count(), 1, "{log}");
        rig.finish();
    }

    #[test]
    fn lost_packets_are_counted_as_concealed() {
        let (rig, device, pipewire) = ready();
        let uplink = device.pods.uplink();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        recorder.wait_frames(frames(SOME), HANG_LIMIT);
        assert_eq!(counts(&rig.status()).concealed, 0);
        uplink.lose(1);
        wait_packets(&uplink, uplink.sent_packets() + 2);
        read_up_to_now(&rig, &device);
        let samples = recorder.finish();
        first_sound(&samples);
        let counted = counts_after_leaving(&rig);
        assert_eq!(
            counted.concealed,
            u64::from(UNITS_PER_PACKET),
            "{counted:?}"
        );
        assert_eq!(counted.decode_errors, 0, "{counted:?}");
        rig.finish();
    }

    #[test]
    fn garbled_units_are_counted_and_the_daemon_lives() {
        let (rig, device, pipewire) = ready();
        let uplink = device.pods.uplink();
        let first = record(&rig, &pipewire, "first.wav");
        wait_mic(&rig, "recording");
        first.wait_frames(frames(SOME), HANG_LIMIT);
        assert_eq!(counts(&rig.status()).decode_errors, 0);
        uplink.garble(GARBLED);
        wait_packets(&uplink, uplink.sent_packets() + u64::from(GARBLED) + 1);
        read_up_to_now(&rig, &device);
        drop(first.finish());
        let counted = counts_after_leaving(&rig);
        // The fake's noise is seeded, so every run garbles the same units, and the decoder refuses each of them.
        let units = u64::from(GARBLED * UNITS_PER_PACKET);
        assert_eq!(
            (counted.decode_errors, counted.concealed),
            (units, units),
            "{counted:?}"
        );
        // The pods' audio still reaches a recorder after the refused units.
        let second = record(&rig, &pipewire, "second.wav");
        wait_mic(&rig, "recording");
        second.wait_frames(frames(AGAIN), HANG_LIMIT);
        first_sound(&second.finish());
        rig.finish();
    }

    #[test]
    fn a_recorder_is_logged_linked_then_left_with_its_counters() {
        let (mut rig, device, pipewire) = ready();
        let uplink = device.pods.uplink();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        recorder.wait_frames(frames(SOME), HANG_LIMIT);
        uplink.lose(1);
        wait_packets(&uplink, uplink.sent_packets() + 2);
        read_up_to_now(&rig, &device);
        drop(recorder.finish());
        let counted = counts_after_leaving(&rig);
        let daemon = rig.daemon();
        let line = super::support::wait_for("the recorder left line", || {
            daemon
                .log()
                .lines()
                .find(|line| line.starts_with(LEFT))
                .map(str::to_owned)
        });
        assert_eq!(
            left_counts(&line),
            (counted.underruns, counted.concealed),
            "{line}"
        );
        assert_eq!(counted.concealed, u64::from(UNITS_PER_PACKET), "{line}");
        let log = daemon.log();
        let linked = log.lines().position(|line| line == LINKED);
        let left = log.lines().position(|line| line.starts_with(LEFT));
        assert!(linked.is_some() && linked < left, "{log}");
        rig.finish();
    }

    #[test]
    fn the_log_holds_no_address_and_no_name() {
        let (mut rig, device, pipewire) = ready();
        let uplink = device.pods.uplink();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        recorder.wait_frames(frames(SOME), HANG_LIMIT);
        uplink.lose(1);
        uplink.garble(GARBLED);
        wait_packets(&uplink, uplink.sent_packets() + u64::from(GARBLED) + 2);
        read_up_to_now(&rig, &device);
        drop(recorder.finish());
        rig.wait_status("the recorder gone", |status| status["mic"] != "recording");
        device.pods.set_ear(Placed::InCase, Placed::InCase);
        wait_mic(&rig, "not_worn");
        device.pods.set_ear(Placed::InEar, Placed::InEar);
        set(rig.session(), "microphone", "off");
        wait_mic(&rig, "off");
        lose(&rig, &device);
        rig.term();
        assert!(rig.exited(HANG_LIMIT).success());
        let log = rig.daemon().log().to_lowercase();
        assert!(log.contains(FIRST_AUDIO_PREFIX), "{log}");
        assert!(log.contains(LEFT), "{log}");
        assert!(!log.contains(&NAME.to_lowercase()), "{log}");
        for form in ADDRESS_FORMS {
            assert!(!log.contains(form), "{log}");
        }
        rig.finish();
    }

    #[test]
    #[should_panic(expected = "the recording of 3 samples is silent; the rig missed its timing")]
    fn a_silent_recording_fails_with_the_rig_s_name() {
        assert_sounds(&[0, 0, 0], "; the rig missed its timing");
    }

    #[test]
    fn one_sample_of_sound_is_not_silent() {
        assert_sounds(&[0, 5, 0], "");
    }
}
