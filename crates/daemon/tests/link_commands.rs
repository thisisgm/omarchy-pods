//! The link's commands through the built daemon: `librepods set`, `librepods ctl` and the socket change the pods, and `watch` shows what the pods change.

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

    use pods_fake::{Fault, Profile};
    use pods_samples::{
        ADAPTIVE_LEVEL, CONVERSATION_AWARENESS, NOISE, ONE_BUD_ANC, Placed, setting,
    };
    use serde_json::Value;

    use super::support::rig::{
        ADAPTIVE, ANC, CONFIRM_TIMEOUT, Device, Rig, TRANSPARENCY, assert_gap, not_before,
    };
    use super::support::{HANG_LIMIT, Proto1, outcome, set_request};

    /// The adaptive level the test sets, in percent.
    const LEVEL: u8 = 40;

    /// Where a control frame carries its opcode and identifier: `09 00 0d` in `04 00 04 00 09 00 0d 02 00 00 00`.
    const CONTROL_ID_BYTES: std::ops::Range<usize> = 4..7;

    /// The most a `sent` reply may take.
    const SENT_WITHIN: Duration = Duration::from_millis(100);

    /// How soon a change at the pods shows in `watch`.
    const SHOWN_WITHIN: Duration = Duration::from_secs(1);

    /// The daemon with pods of `profile` and `fault`, their link up and their name in the status.
    fn up(profile: Profile, fault: Fault) -> (Rig, Device) {
        let mut rig = Rig::new();
        let device = rig.add_pods(profile);
        device.pods.fault(fault);
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        (rig, device)
    }

    /// Waits for `noise_mode` in the status to be `status`.
    fn wait_noise(rig: &Rig, status: u8) {
        rig.wait_status("the noise mode", |current| current["noise_mode"] == status);
    }

    /// Every frame the pods heard that carries the noise control identifier.
    fn noise_frames(device: &Device) -> Vec<Vec<u8>> {
        let noise = [0x09, 0x00, NOISE];
        device
            .pods
            .heard()
            .into_iter()
            .map(|heard| heard.sdu)
            .filter(|sdu| sdu.get(CONTROL_ID_BYTES) == Some(&noise[..]))
            .collect()
    }

    /// Fails unless `librepods <args>` exited 1 with `message` on stderr.
    fn refused(rig: &Rig, args: &[&str], message: &str) {
        let output = rig.client(args);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!("{message}\n")
        );
    }

    fn in_ear(status: &Value) -> [bool; 2] {
        ["left", "right"].map(|side| status[side]["in_ear"].as_bool().unwrap())
    }

    #[test]
    fn set_noise_anc_writes_the_frame_and_is_confirmed() {
        let (rig, device) = up(Profile::default(), Fault::None);
        let output = rig.client(&["set", "noise", "anc"]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(noise_frames(&device), [setting(NOISE, ANC.wire)]);
        assert_eq!(rig.status()["noise_mode"], ANC.status);
        let reply = Proto1::connect(rig.session()).ask(&set_request(1, "noise", "transparency"));
        assert_eq!(outcome(&reply), "confirmed", "{reply}");
        rig.finish();
    }

    #[test]
    fn a_setting_the_pods_never_report_is_timeout_inside_the_client_wait() {
        let (rig, device) = up(Profile::default(), Fault::NeverConfirm);
        let asked = Instant::now();
        refused(
            &rig,
            &["set", "noise", "anc"],
            "The AirPods did not answer in time.",
        );
        assert_gap(
            "the timeout after the command",
            asked,
            Instant::now(),
            &not_before(CONFIRM_TIMEOUT),
        );
        assert_eq!(noise_frames(&device), [setting(NOISE, ANC.wire)]);
        rig.finish();
    }

    #[test]
    fn noise_with_no_bud_in_an_ear_is_not_worn() {
        let profile = Profile {
            primary: Placed::OutOfEar,
            secondary: Placed::OutOfEar,
            ..Profile::default()
        };
        let (rig, device) = up(profile, Fault::None);
        refused(
            &rig,
            &["set", "noise", "anc"],
            "The AirPods are not in an ear.",
        );
        // Written and never answered: the refusal comes from the confirmation wait.
        assert_eq!(noise_frames(&device), [setting(NOISE, ANC.wire)]);
        rig.finish();
    }

    #[test]
    fn off_on_a_model_without_off_is_refused_and_nothing_is_written() {
        let (rig, device) = up(Profile::default(), Fault::None);
        refused(
            &rig,
            &["set", "noise", "off"],
            "These AirPods do not support that.",
        );
        assert_eq!(noise_frames(&device), Vec::<Vec<u8>>::new());
        rig.finish();
    }

    #[test]
    fn legacy_noise_cycle_skips_the_modes_the_model_lacks() {
        let profile = Profile {
            noise: ADAPTIVE.wire,
            ..Profile::default()
        };
        let (rig, device) = up(profile, Fault::None);
        wait_noise(&rig, ADAPTIVE.status);
        let output = rig.client(&["ctl", "noise:cycle"]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        wait_noise(&rig, ANC.status);
        assert_eq!(noise_frames(&device), [setting(NOISE, ANC.wire)]);
        rig.finish();
    }

    #[test]
    fn conversational_awareness_and_one_bud_are_confirmed_by_their_reports() {
        let (rig, device) = up(Profile::default(), Fault::None);
        let mut client = Proto1::connect(rig.session());
        let switches = [
            (
                "conversational_awareness",
                "conversational_awareness",
                CONVERSATION_AWARENESS,
            ),
            ("one_bud_anc", "one_bud_anc_mode", ONE_BUD_ANC),
        ];
        let mut id = 0;
        for (key, status_key, identifier) in switches {
            for (value, on, wire) in [("true", true, 1), ("false", false, 2)] {
                id += 1;
                let reply = client.ask(&set_request(id, key, value));
                assert_eq!(outcome(&reply), "confirmed", "{key} {value}: {reply}");
                device
                    .pods
                    .wait_heard(&setting(identifier, wire), 1, Duration::ZERO)
                    .unwrap_or_else(|_| panic!("{key} {value}: no frame heard"));
                assert_eq!(rig.status()[status_key], on, "{key} {value}");
            }
        }
        rig.finish();
    }

    #[test]
    fn adaptive_level_is_sent_at_once() {
        let profile = Profile {
            noise: ADAPTIVE.wire,
            ..Profile::default()
        };
        // The pods never report the level back, so the status can only show the one the daemon stored.
        let (rig, device) = up(profile, Fault::NeverConfirm);
        wait_noise(&rig, ADAPTIVE.status);
        let asked = Instant::now();
        let request = set_request(1, "adaptive_level", &LEVEL.to_string());
        let reply = Proto1::connect(rig.session()).ask(&request);
        assert_gap(
            "the reply to the adaptive level",
            asked,
            Instant::now(),
            &(Duration::ZERO..=SENT_WITHIN),
        );
        assert_eq!(outcome(&reply), "sent", "{reply}");
        device
            .pods
            .wait_heard(&setting(ADAPTIVE_LEVEL, LEVEL), 1, HANG_LIMIT)
            .unwrap_or_else(|_| panic!("no adaptive level frame heard"));
        assert_eq!(rig.status()["adaptive_noise_level"], LEVEL);
        rig.finish();
    }

    #[test]
    fn a_mode_changed_at_the_pods_appears_in_watch() {
        let (rig, device) = up(Profile::default(), Fault::None);
        let watch = rig.watch();
        watch.lines_until(&rig.status());
        let changed = Instant::now();
        device.pods.set_setting(NOISE, TRANSPARENCY.wire);
        let printed = watch.next(SHOWN_WITHIN).expect("a line within a second");
        assert_gap(
            "the line after the change at the pods",
            changed,
            printed.at,
            &(Duration::ZERO..=SHOWN_WITHIN),
        );
        assert_eq!(printed.state["noise_mode"], TRANSPARENCY.status);
        rig.finish();
    }

    #[test]
    fn a_bud_taken_out_appears_in_watch() {
        let (rig, device) = up(Profile::default(), Fault::None);
        let watch = rig.watch();
        let before = watch.lines_until(&rig.status());
        assert_eq!(before.last().map(in_ear), Some([true, true]));
        device.pods.set_ear(Placed::OutOfEar, Placed::InEar);
        let printed = watch
            .next(HANG_LIMIT)
            .expect("a line after the bud came out");
        let out = in_ear(&printed.state)
            .iter()
            .filter(|in_ear| !**in_ear)
            .count();
        assert_eq!(out, 1, "{}", printed.state);
        rig.finish();
    }
}
