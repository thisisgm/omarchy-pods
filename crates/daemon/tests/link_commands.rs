//! The link's commands through the built daemon: `pods set`, `pods ctl` and the socket change the pods, and `watch` shows what the pods change.

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

    use pods_core::link::REPORT_HOLD_MS;
    use pods_fake::{Fault, Profile};
    use pods_samples::{
        ADAPTIVE_LEVEL, CONVERSATION_AWARENESS, NOISE, ONE_BUD_ANC, Placed, setting,
    };
    use serde_json::Value;

    use super::support::rig::{ADAPTIVE, ANC, Device, Mode, Rig, TRANSPARENCY, assert_gap};
    use super::support::watch::{Printed, Watch};
    use super::support::{HANG_LIMIT, Proto1, outcome, set_request};

    /// A noise value no mode has, which the daemon must refuse wherever it lands.
    const NO_MODE: u8 = 0xff;
    /// The adaptive level the test sets, in percent.
    const LEVEL: u8 = 40;

    /// Where a control frame carries its opcode and identifier: `09 00 0d` in `04 00 04 00 09 00 0d 02 00 00 00`.
    const CONTROL_ID_BYTES: std::ops::Range<usize> = 4..7;

    /// The most a `sent` reply may take.
    const SENT_WITHIN: Duration = Duration::from_millis(100);

    /// How soon a change at the pods shows in `watch`.
    const SHOWN_WITHIN: Duration = Duration::from_secs(1);

    /// Noise writes asked one after another, each timed from request to reply.
    const NOISE_WRITES: usize = 22;

    /// Far more writes than a socket pair's send buffer takes before it is full.
    const WRITES_MAX: usize = 100_000;

    /// The two modes the noise tests alternate between, with the word `set noise` takes.
    const MODES: [(&str, Mode); 2] = [("anc", ANC), ("transparency", TRANSPARENCY)];

    /// The default pods, in transparency.
    fn in_transparency() -> Profile {
        Profile {
            noise: TRANSPARENCY.wire,
            ..Profile::default()
        }
    }

    /// Every line `watch` prints until one shows a bud out of its ear.
    fn lines_until_a_bud_is_out(watch: &Watch) -> Vec<Value> {
        let mut lines = Vec::new();
        while lines
            .last()
            .is_none_or(|state| in_ear(state) == [true, true])
        {
            let printed = watch
                .next(HANG_LIMIT)
                .unwrap_or_else(|| panic!("no bud out; watch printed {lines:#?}"));
            lines.push(printed.state);
        }
        lines
    }

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

    /// Every state `watch` printed from the first in `kept` shows `kept`, up to `taken`, the held report, which the reader took no sooner than a hold after `asked`.
    fn assert_kept_until(watch: &Watch, rig: &Rig, kept: Mode, taken: Mode, asked: Instant) {
        let shown: Vec<Printed> = watch
            .printed_until(&rig.status())
            .into_iter()
            .skip_while(|printed| printed.state["noise_mode"] != kept.status)
            .collect();
        let modes: Vec<&Value> = shown
            .iter()
            .map(|printed| &printed.state["noise_mode"])
            .collect();
        let Some(released) = modes.iter().position(|mode| **mode == taken.status) else {
            panic!("watch never showed {taken:?} after {kept:?}: {modes:?}");
        };
        assert!(
            modes[..released].iter().all(|mode| **mode == kept.status),
            "{modes:?}"
        );
        let hold = Duration::from_millis(REPORT_HOLD_MS);
        assert_gap(
            "the held report shown",
            asked,
            shown[released].at,
            &(hold..),
        );
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

    /// Fails unless `pods <args>` exited 1 with `message` on stderr.
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
    fn set_noise_anc_writes_the_frame_and_shows_the_mode() {
        let (rig, device) = up(Profile::default(), Fault::None);
        let output = rig.client(&["set", "noise", "anc"]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(rig.status()["noise_mode"], ANC.status);
        device
            .pods
            .wait_heard(&setting(NOISE, ANC.wire), 1, HANG_LIMIT)
            .unwrap_or_else(|_| panic!("{:?}", noise_frames(&device)));
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
        // A switch written after it is heard after anything written before it.
        let output = rig.client(&["set", "conversational_awareness", "true"]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        device
            .pods
            .wait_heard(&setting(CONVERSATION_AWARENESS, 1), 1, HANG_LIMIT)
            .unwrap_or_else(|_| panic!("no switch frame heard"));
        assert_eq!(noise_frames(&device), Vec::<Vec<u8>>::new());
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

    #[test]
    #[expect(
        clippy::print_stderr,
        reason = "the range of reply times goes into the test's output"
    )]
    fn a_noise_mode_is_sent_shown_at_once_and_written_on_every_ask() {
        // The pods never report a write back, as the measured model often reports one late.
        let (rig, device) = up(in_transparency(), Fault::NeverConfirm);
        let mut client = Proto1::connect(rig.session());
        let mut waits = Vec::new();
        for request in 0..=NOISE_WRITES {
            // The last ask repeats the mode just written, which the state already shows.
            let (word, mode) = MODES[request.min(NOISE_WRITES - 1) % MODES.len()];
            let asked = Instant::now();
            let reply = client.ask(&set_request(request, "noise", word));
            waits.push(asked.elapsed());
            assert_eq!(outcome(&reply), "sent", "request {request}: {reply}");
            assert_eq!(rig.status()["noise_mode"], mode.status, "request {request}");
        }
        for (count, mode) in [
            (NOISE_WRITES / 2, ANC),
            (NOISE_WRITES / 2 + 1, TRANSPARENCY),
        ] {
            device
                .pods
                .wait_heard(&setting(NOISE, mode.wire), count, HANG_LIMIT)
                .unwrap_or_else(|_| panic!("{:?}", noise_frames(&device)));
        }
        assert_eq!(noise_frames(&device).len(), NOISE_WRITES + 1);
        let (fastest, slowest) = (waits.iter().min(), waits.iter().max());
        eprintln!(
            "{} replies came {fastest:?} to {slowest:?} after their request",
            waits.len()
        );
        rig.finish();
    }

    #[test]
    fn a_switch_the_pods_never_report_is_sent_shown_and_written_both_ways() {
        let (rig, device) = up(Profile::default(), Fault::NeverConfirm);
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
                assert_eq!(outcome(&reply), "sent", "{key} {value}: {reply}");
                assert_eq!(rig.status()[status_key], on, "{key} {value}");
                device
                    .pods
                    .wait_heard(&setting(identifier, wire), 1, HANG_LIMIT)
                    .unwrap_or_else(|_| panic!("{key} {value}: no frame heard"));
            }
        }
        rig.finish();
    }

    #[test]
    fn a_late_report_of_an_earlier_write_never_shows_after_a_later_one() {
        let (rig, device) = up(in_transparency(), Fault::NeverConfirm);
        let watch = rig.watch();
        let mut client = Proto1::connect(rig.session());
        for (id, word) in [(1, "anc"), (2, "adaptive")] {
            let reply = client.ask(&set_request(id, "noise", word));
            assert_eq!(outcome(&reply), "sent", "{word}: {reply}");
        }
        watch.lines_until(&rig.status());
        // Both reports come late and in order; the bud taken out after them marks the end.
        device.pods.set_setting(NOISE, ANC.wire);
        device.pods.set_setting(NOISE, ADAPTIVE.wire);
        device.pods.set_ear(Placed::OutOfEar, Placed::InEar);
        let shown: Vec<Value> = lines_until_a_bud_is_out(&watch)
            .iter()
            .map(|state| state["noise_mode"].clone())
            .collect();
        assert!(
            shown.iter().all(|mode| *mode == ADAPTIVE.status),
            "{shown:?}"
        );
        rig.finish();
    }

    #[test]
    fn a_report_held_back_is_taken_when_the_hold_of_a_later_write_passes() {
        let (rig, device) = up(in_transparency(), Fault::NeverConfirm);
        let mut client = Proto1::connect(rig.session());
        assert_eq!(
            outcome(&client.ask(&set_request(1, "noise", "anc"))),
            "sent"
        );
        // The bud taken out after the report marks it read and held back.
        device.pods.set_setting(NOISE, ADAPTIVE.wire);
        device.pods.set_ear(Placed::OutOfEar, Placed::InEar);
        rig.wait_status("a bud out", |status| in_ear(status) != [true, true]);
        assert_eq!(rig.status()["noise_mode"], ANC.status);

        let watch = rig.watch();
        watch.lines_until(&rig.status());
        // Stamped before the ask, so the daemon's hold cannot start earlier than this.
        let asked = Instant::now();
        // A fresh client: the daemon closes one left idle past its stall deadline.
        let reply = Proto1::connect(rig.session()).ask(&set_request(2, "noise", "transparency"));
        assert_eq!(outcome(&reply), "sent", "{reply}");
        wait_noise(&rig, ADAPTIVE.status);
        assert_kept_until(&watch, &rig, TRANSPARENCY, ADAPTIVE, asked);
        rig.finish();
    }

    #[test]
    fn an_invalid_report_inside_a_hold_leaves_the_valid_one_it_held_back() {
        let (rig, device) = up(in_transparency(), Fault::NeverConfirm);
        let mut client = Proto1::connect(rig.session());
        let watch = rig.watch();
        watch.lines_until(&rig.status());
        // Stamped before the ask, so the daemon's hold cannot start earlier than this.
        let asked = Instant::now();
        let reply = client.ask(&set_request(1, "noise", "anc"));
        assert_eq!(outcome(&reply), "sent", "{reply}");
        device.pods.set_setting(NOISE, ADAPTIVE.wire);
        device.pods.set_setting(NOISE, NO_MODE);
        // The bud taken out after both reports marks them read inside the hold.
        device.pods.set_ear(Placed::OutOfEar, Placed::InEar);
        rig.wait_status("a bud out", |status| in_ear(status) != [true, true]);
        wait_noise(&rig, ADAPTIVE.status);
        assert_kept_until(&watch, &rig, ANC, ADAPTIVE, asked);
        rig.finish();
    }

    #[test]
    fn a_write_that_fails_is_not_connected_and_changes_nothing() {
        let (rig, device) = up(in_transparency(), Fault::None);
        device.pods.stop_reading();
        let mut client = Proto1::connect(rig.session());
        let mut shown = TRANSPARENCY.status;
        let mut request = 0;
        let refused = loop {
            assert!(request < WRITES_MAX, "{request} writes and none failed");
            let (word, mode) = MODES[request % MODES.len()];
            let reply = client.ask(&set_request(request, "noise", word));
            if outcome(&reply) != "sent" {
                break reply;
            }
            shown = mode.status;
            request += 1;
        };
        assert_eq!(outcome(&refused), "not_connected", "{refused}");
        let status = rig.status();
        assert_eq!(status["noise_mode"], shown, "{status}");
        rig.finish();
    }

    #[test]
    fn a_report_of_another_value_is_taken_once_the_written_one_came_or_when_the_hold_passes() {
        let (rig, device) = up(in_transparency(), Fault::NeverConfirm);
        let mut client = Proto1::connect(rig.session());
        assert_eq!(
            outcome(&client.ask(&set_request(1, "noise", "anc"))),
            "sent"
        );
        let first_replied = Instant::now();
        let hold = Duration::from_millis(REPORT_HOLD_MS);
        device.pods.set_setting(NOISE, ANC.wire);
        device.pods.set_setting(NOISE, ADAPTIVE.wire);
        wait_noise(&rig, ADAPTIVE.status);
        // The written value's report ended the hold, so the next was taken long before the hold's end.
        assert_gap(
            "adaptive shown",
            first_replied,
            Instant::now(),
            &(Duration::ZERO..=hold / 2),
        );

        let watch = rig.watch();
        watch.lines_until(&rig.status());
        // Stamped before the ask, so the daemon's hold cannot start earlier than this.
        let asked = Instant::now();
        // A fresh client: the daemon closes one left idle past its stall deadline.
        let reply = Proto1::connect(rig.session()).ask(&set_request(2, "noise", "transparency"));
        assert_eq!(outcome(&reply), "sent", "{reply}");
        device.pods.set_setting(NOISE, ANC.wire);
        device.pods.set_ear(Placed::OutOfEar, Placed::InEar);
        rig.wait_status("a bud out", |status| in_ear(status) != [true, true]);
        wait_noise(&rig, ANC.status);
        assert_kept_until(&watch, &rig, TRANSPARENCY, ANC, asked);
        rig.finish();
    }
}
