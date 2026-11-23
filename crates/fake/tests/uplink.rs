//! Support tests: the fake pods' uplink over a socket pair, each time check held to the fake's own schedule; no user path yet.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod support;

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use pods_fake::uplink::{
        DEFAULT_LEVEL, MARK_LINE, NOT_WORN_PACKETS, PERIOD, TONE_LINE, UNITS_PER_PACKET, Uplink,
    };
    use pods_fake::{Fault, Pods, Profile};
    use pods_mic::decode::{Decoded, Decoder};
    use pods_mic::{RATE_HZ, UPLINK_PERIOD_MS};
    use pods_samples::Placed;
    use pods_samples::uplink::{
        STAMP_STEP, START, STOP, audio_sdu, session_marker, state_marker, tone_unit,
    };
    use pods_sys::l2cap::Channel;

    use super::support::{
        REAL_TIME_SLACK, RECV_TIMEOUT, ask, assert_quiet, open, opening, recv, recv_many,
    };

    /// The fake's cadence is the one the decode chain counts on: a packet's units hold one period of samples.
    #[expect(clippy::as_conversions, reason = "a const context has no `From`")]
    const _: () = assert!(
        PERIOD.as_millis() == UPLINK_PERIOD_MS as u128
            && (UNITS_PER_PACKET * STAMP_STEP) as u128 * 1000
                == RATE_HZ as u128 * PERIOD.as_millis()
    );

    /// The accessory header and the opcode, which the state marker shares with no other packet of the burst.
    const OPCODE_END: usize = 6;

    /// The pods on one channel and the host's end of it; the drop ends every serving thread and waits for it on every way out.
    struct Served {
        pods: Pods,
        uplink: Uplink,
        host: Channel,
    }

    impl Served {
        fn new(profile: Profile) -> Self {
            let pods = Pods::new(profile);
            let host = open(&pods);
            Self {
                uplink: pods.uplink(),
                pods,
                host,
            }
        }
    }

    impl Drop for Served {
        fn drop(&mut self) {
            let finished = self.pods.clone().finish();
            if !thread::panicking() {
                finished.unwrap();
            }
        }
    }

    fn markers(running: bool) -> [Vec<u8>; 2] {
        [
            state_marker(running).to_vec(),
            session_marker(running).to_vec(),
        ]
    }

    /// The audio packet at place `slot` of the schedule: four units of `line` at `level`, stamped from the START.
    fn audio(slot: u32, line: u16, level: u8) -> Vec<u8> {
        let units = vec![tone_unit(line, level); usize::try_from(UNITS_PER_PACKET).unwrap()];
        audio_sdu(slot.wrapping_mul(UNITS_PER_PACKET * STAMP_STEP), &units)
    }

    fn tone(slot: u32) -> Vec<u8> {
        audio(slot, TONE_LINE, DEFAULT_LEVEL)
    }

    /// When the schedule of a run the fake started at `started` sends the packet at `slot`.
    fn due(started: Instant, slot: u32) -> Instant {
        let after = PERIOD.saturating_mul(slot.saturating_add(1));
        started.checked_add(after).unwrap()
    }

    fn recv_at(host: &Channel) -> (Vec<u8>, Instant) {
        let sdu = recv(host);
        (sdu, Instant::now())
    }

    /// Sends a START, takes both running markers and returns when the fake heard it, which is where its schedule counts from.
    fn start(served: &Served) -> Instant {
        let heard = served.pods.heard();
        let before = heard.iter().filter(|heard| heard.sdu == START).count();
        ask(&served.host, &START);
        assert_eq!(recv_many(&served.host, 2), markers(true));
        served
            .pods
            .wait_heard(&START, before.saturating_add(1), RECV_TIMEOUT)
            .unwrap()[before]
            .at
    }

    /// Never before `due`, which the fake's own clock rules out, and within the slack after it.
    fn assert_on_time(what: &str, at: Instant, due: Instant) {
        assert!(
            due <= at && at.saturating_duration_since(due) <= REAL_TIME_SLACK,
            "{what} came {:?} after its time and {:?} before it",
            at.saturating_duration_since(due),
            due.saturating_duration_since(at)
        );
    }

    /// Takes the tone packet at `slot`, holds its arrival to the schedule and returns it.
    fn recv_on_time(served: &Served, started: Instant, slot: u32) -> Instant {
        let (sdu, at) = recv_at(&served.host);
        assert_eq!(sdu, tone(slot), "packet {slot}");
        assert_on_time(&format!("packet {slot}"), at, due(started, slot));
        at
    }

    /// Takes tone packets from place `from` on until both stopped markers; returns the place after the last packet and when the markers came.
    fn recv_until_stopped(served: &Served, from: u32) -> (u32, Instant) {
        // Twice the packets the slack lets a STOP lag by, so a fake that never stops fails rather than hangs.
        let lag = REAL_TIME_SLACK.saturating_mul(2).as_millis();
        let at_most = u32::try_from(lag.checked_div(PERIOD.as_millis()).unwrap()).unwrap();
        for slot in from..from.saturating_add(at_most) {
            let (sdu, at) = recv_at(&served.host);
            if sdu == state_marker(false) {
                assert_eq!(recv(&served.host), session_marker(false));
                return (slot, at);
            }
            assert_eq!(sdu, tone(slot), "packet {slot}");
        }
        panic!("no stopped markers within {at_most} packets of packet {from}");
    }

    fn heard_at(served: &Served, sdu: &[u8]) -> Instant {
        served.pods.wait_heard(sdu, 1, RECV_TIMEOUT).unwrap()[0].at
    }

    #[test]
    #[expect(
        clippy::print_stderr,
        reason = "the spacing the test saw goes into its output"
    )]
    fn a_start_is_answered_by_markers_then_audio_every_30_ms() {
        const PACKETS: u32 = 40;
        let served = Served::new(Profile::default());
        assert!(!served.uplink.running());
        let started = start(&served);
        assert!(served.uplink.running());
        let mut arrivals = Vec::new();
        for slot in 0..PACKETS {
            if slot == PACKETS / 2 {
                // A START while the uplink runs changes nothing: the packets after it stay in step, with no marker between.
                ask(&served.host, &START);
            }
            arrivals.push(recv_on_time(&served, started, slot));
        }
        let spacings = arrivals.windows(2).map(|pair| pair[1] - pair[0]);
        let (closest, furthest) = (spacings.clone().min(), spacings.max());
        eprintln!("{PACKETS} packets came {closest:?} to {furthest:?} apart");
        let gap = served.uplink.largest_gap();
        assert!(gap <= PERIOD + REAL_TIME_SLACK, "{gap:?}");
        assert!(served.uplink.sent_packets() >= u64::from(PACKETS));
        let starts = served.pods.wait_heard(&START, 2, RECV_TIMEOUT).unwrap();
        assert_eq!(starts.len(), 2);
        let last = arrivals[arrivals.len() - 1];
        assert!(
            starts[1].at < last,
            "the second START was heard after the last packet"
        );
    }

    #[test]
    fn a_held_start_is_silent_until_released() {
        let served = Served::new(Profile::default());
        served.uplink.hold_starts(true);
        ask(&served.host, &START);
        heard_at(&served, &START);
        assert_quiet(&served.host);
        assert!(!served.uplink.running());
        let before = Instant::now();
        served.uplink.release_start();
        let after = Instant::now();
        assert_eq!(recv_many(&served.host, 2), markers(true));
        assert!(served.uplink.running());
        let (first, at) = recv_at(&served.host);
        assert_eq!(first, tone(0));
        // The schedule counts from the release, which stands between the two stamps.
        assert!(
            before + PERIOD <= at && at <= after + PERIOD + REAL_TIME_SLACK,
            "{:?} after the release",
            at - before
        );
        // With no START held a release changes nothing.
        served.uplink.release_start();
        assert_eq!(recv(&served.host), tone(1));
    }

    #[test]
    fn a_stop_sends_at_most_two_packets_then_the_markers() {
        const BEFORE: u32 = 3;
        let served = Served::new(Profile::default());
        let started = start(&served);
        for slot in 0..BEFORE {
            recv_on_time(&served, started, slot);
        }
        ask(&served.host, &STOP);
        let (sent, stopped) = recv_until_stopped(&served, BEFORE);
        let heard = heard_at(&served, &STOP);
        // The third packet from the end was due before the STOP was heard, so no more than two went out after it.
        assert!(
            due(started, sent - 3) <= heard,
            "{sent} packets, the STOP heard {:?} into the run",
            heard - started
        );
        assert_on_time("the stopped markers", stopped, due(started, sent - 1));
        assert_quiet(&served.host);
        assert!(!served.uplink.running());
        assert_eq!(served.uplink.sent_packets(), u64::from(sent));
    }

    #[test]
    fn a_held_stop_keeps_the_audio_until_released() {
        /// More packets than follow a STOP that is answered.
        const KEPT: u32 = 5;
        let served = Served::new(Profile::default());
        served.uplink.hold_stops(true);
        let started = start(&served);
        ask(&served.host, &STOP);
        let heard = heard_at(&served, &STOP);
        let (mut slot, mut after_the_stop) = (0, 0);
        while after_the_stop < KEPT {
            recv_on_time(&served, started, slot);
            // A packet due after the STOP was heard went out after it.
            after_the_stop += u32::from(due(started, slot) > heard);
            slot += 1;
        }
        served.uplink.release_stop();
        let released = Instant::now();
        let (sent, _) = recv_until_stopped(&served, slot);
        assert!(
            due(started, sent - 3) <= released,
            "{sent} packets, released {:?} into the run",
            released - started
        );
        assert_quiet(&served.host);
        assert!(!served.uplink.running());
    }

    #[test]
    fn never_answer_hears_the_start_and_says_nothing() {
        let served = Served::new(Profile::default());
        served.uplink.never_answer(true);
        ask(&served.host, &START);
        heard_at(&served, &START);
        assert_quiet(&served.host);
        assert!(!served.uplink.running());
        assert_eq!(served.uplink.sent_packets(), 0);
        served.uplink.never_answer(false);
        let started = start(&served);
        recv_on_time(&served, started, 0);
    }

    #[test]
    fn no_bud_worn_sends_five_packets() {
        let served = Served::new(Profile {
            primary: Placed::OutOfEar,
            secondary: Placed::InCase,
            ..Profile::default()
        });
        let started = start(&served);
        for slot in 0..NOT_WORN_PACKETS {
            recv_on_time(&served, started, slot);
        }
        assert_quiet(&served.host);
        assert!(served.uplink.running());
        assert_eq!(
            served.uplink.sent_packets(),
            u64::from(NOT_WORN_PACKETS),
            "five packets"
        );
        ask(&served.host, &STOP);
        assert_eq!(recv_many(&served.host, 2), markers(false));
        assert_quiet(&served.host);
        assert!(!served.uplink.running());
    }

    #[test]
    fn a_stall_then_a_burst_keeps_every_stamp() {
        /// Twice the slack, so a schedule the stall moved is told from the one the START set.
        const GAP: Duration = REAL_TIME_SLACK.saturating_mul(2);
        const BEFORE: u32 = 4;
        /// The packets before the stall, the one held, those that come due inside the gap and some after the burst.
        const PACKETS: u32 = 50;
        let served = Served::new(Profile::default());
        let started = start(&served);
        let mut arrivals: Vec<Instant> = (0..BEFORE)
            .map(|slot| recv_on_time(&served, started, slot))
            .collect();
        served.uplink.stall(GAP);
        for slot in BEFORE..PACKETS {
            let (sdu, at) = recv_at(&served.host);
            assert_eq!(sdu, tone(slot), "packet {slot}");
            arrivals.push(at);
        }
        let gap = served.uplink.largest_gap();
        assert!(GAP <= gap && gap <= GAP + REAL_TIME_SLACK, "{gap:?}");

        let spacing = |slot: &usize| arrivals[*slot] - arrivals[*slot - 1];
        let held = (1..arrivals.len()).max_by_key(spacing).unwrap();
        assert!(
            spacing(&held) >= GAP.saturating_sub(REAL_TIME_SLACK),
            "{:?}",
            spacing(&held)
        );
        // The packet before the held one went out no sooner than its time, so every packet due within the gap after that time left with the held one.
        let together = usize::try_from(GAP.as_millis() / PERIOD.as_millis()).unwrap();
        let burst = arrivals[held + together - 1] - arrivals[held];
        assert!(burst <= REAL_TIME_SLACK, "the burst took {burst:?}");
        let last = PACKETS - 1;
        assert_on_time(
            "the last packet",
            arrivals[arrivals.len() - 1],
            due(started, last),
        );
    }

    #[test]
    fn lost_packets_leave_a_stamp_gap() {
        const LOST: u32 = 3;
        const BEFORE: u32 = 2;
        const AFTER: u32 = 6;
        let served = Served::new(Profile::default());
        let started = start(&served);
        for slot in 0..BEFORE {
            recv_on_time(&served, started, slot);
        }
        served.uplink.lose(LOST);
        let lost = Instant::now();
        // The loss takes the next packets the fake makes, so the stamps say where it fell.
        let (mut slot, mut holes) = (BEFORE, Vec::new());
        for _ in 0..AFTER {
            let (sdu, at) = recv_at(&served.host);
            let found = (slot..=slot + LOST).find(|&place| sdu == tone(place));
            let found = found.unwrap_or_else(|| panic!("no packet of places {slot} on"));
            assert_on_time(&format!("packet {found}"), at, due(started, found));
            if found != slot {
                // The packet before the hole was made before the loss, and never before its time.
                assert!(
                    due(started, slot - 1) <= lost,
                    "the hole opens at packet {slot}"
                );
                holes.push(found - slot);
            }
            slot = found + 1;
        }
        assert_eq!(holes, [LOST]);
    }

    #[test]
    fn stop_unasked_sends_the_stopped_markers() {
        const BEFORE: u32 = 2;
        let served = Served::new(Profile::default());
        let started = start(&served);
        for slot in 0..BEFORE {
            recv_on_time(&served, started, slot);
        }
        served.uplink.stop_unasked();
        let (sent, _) = recv_until_stopped(&served, BEFORE);
        assert_quiet(&served.host);
        assert!(!served.uplink.running());
        assert_eq!(served.uplink.sent_packets(), u64::from(sent));
        let heard = served.pods.heard();
        assert!(heard.iter().all(|heard| heard.sdu != STOP), "{heard:?}");
        // The next run stamps its packets from its own START.
        let started = start(&served);
        recv_on_time(&served, started, 0);
    }

    #[test]
    fn the_first_burst_holds_the_stopped_marker_of_13_bytes() {
        let served = Served::new(Profile::default());
        let burst = opening(&served.host);
        let stopped = state_marker(false).to_vec();
        let of_its_opcode: Vec<&Vec<u8>> = burst
            .iter()
            .filter(|sdu| sdu.starts_with(&stopped[..OPCODE_END]))
            .collect();
        assert_eq!(of_its_opcode, [&stopped]);
        assert_eq!(stopped.len(), 13);
        assert!(!served.uplink.running());
        assert_quiet(&served.host);
    }

    #[test]
    fn a_channel_that_acknowledges_nothing_does_not_answer_a_start() {
        let served = Served::new(Profile::default());
        served.pods.fault(Fault::NeverAcknowledge);
        let mute = open(&served.pods);
        ask(&mute, &START);
        heard_at(&served, &START);
        assert_quiet(&mute);
        assert!(!served.uplink.running());
    }

    #[test]
    fn the_run_ends_with_its_channel() {
        let served = Served::new(Profile::default());
        let second = open(&served.pods);
        let newest = open(&served.pods);
        ask(&second, &START);
        assert_eq!(recv_many(&second, 2), markers(true));
        assert_eq!(recv(&second), tone(0));
        assert!(served.uplink.running());
        drop(second);
        served.pods.wait_closed(1, RECV_TIMEOUT).unwrap();
        assert!(!served.uplink.running());
        // The audio went to the channel of the START alone, neither the first nor the newest.
        assert_quiet(&served.host);
        assert_quiet(&newest);
        let started = start(&served);
        recv_on_time(&served, started, 0);
    }

    /// The spectral line and the level of a packet's four units.
    type Kind = (u16, u8);

    /// The samples one decoder gives for packets of four units of each `(line, level)` in turn, a packet a row.
    fn decoded(packets: &[Kind]) -> Vec<Vec<f32>> {
        let mut decoder = Decoder::new().unwrap();
        let mut rows = Vec::new();
        for &(line, level) in packets {
            let mut row = Vec::new();
            for _ in 0..UNITS_PER_PACKET {
                match decoder.unit(&tone_unit(line, level)) {
                    Decoded::Samples(samples) => row.extend_from_slice(samples),
                    refused @ (Decoded::Error | Decoded::Panic) => panic!("{refused:?}"),
                }
            }
            rows.push(row);
        }
        rows
    }

    fn peak(samples: &[f32]) -> f32 {
        samples
            .iter()
            .fold(0.0, |peak, sample| peak.max(sample.abs()))
    }

    /// How often `samples` change sign, which a tone does twice a turn.
    fn crossings(samples: &[f32]) -> usize {
        let signs = samples.windows(2);
        let crossed =
            |pair: &&[f32]| matches!(pair, [before, after] if (*before < 0.0) != (*after < 0.0));
        signs.filter(crossed).count()
    }

    #[test]
    fn the_units_decode() {
        const MARKED: u32 = 3;
        /// Four steps of a quarter of a doubling each: half the amplitude.
        const QUIETER: u8 = DEFAULT_LEVEL - 4;
        const EACH: usize = 4;
        /// A hundredth of full scale, far above a silent unit's nothing.
        const AUDIBLE: f32 = 0.01;
        /// A tone that fills a packet crosses zero twice a turn, give or take its two ends.
        const CROSSINGS_SLACK: usize = 4;
        /// Far more packets than the whole test takes, so a fake that ignores the mark or the level fails rather than hangs.
        const AT_MOST: usize = 100;
        /// The amplitude `QUIETER` leaves.
        const HALF: f32 = 0.5;
        /// How far the decoded ratio may stray from `HALF`.
        const RATIO_SLACK: f32 = 0.01;
        let kinds = [
            (TONE_LINE, DEFAULT_LEVEL),
            (MARK_LINE, DEFAULT_LEVEL),
            (TONE_LINE, QUIETER),
        ];
        let served = Served::new(Profile::default());
        start(&served);
        let mut heard: Vec<Kind> = Vec::new();
        let mut take = |until: &dyn Fn(&[Kind]) -> bool| {
            while !until(&heard) {
                assert!(
                    heard.len() < AT_MOST,
                    "{AT_MOST} packets since the START and the change not seen"
                );
                let sdu = recv(&served.host);
                let slot = u32::try_from(heard.len()).unwrap();
                let kind = kinds
                    .iter()
                    .find(|(line, level)| sdu == audio(slot, *line, *level));
                heard.push(
                    *kind.unwrap_or_else(|| panic!("packet {slot} is no packet of the fake")),
                );
            }
        };
        let ends_with = |kind: Kind| {
            move |heard: &[Kind]| heard.len() >= EACH && heard[heard.len() - EACH..] == [kind; EACH]
        };
        take(&ends_with(kinds[0]));
        served.uplink.mark(MARKED);
        take(&|heard| heard.contains(&kinds[1]));
        take(&ends_with(kinds[0]));
        served.uplink.set_level(QUIETER);
        take(&ends_with(kinds[2]));

        let marked: Vec<usize> = (0..heard.len())
            .filter(|&at| heard[at] == kinds[1])
            .collect();
        let first = marked[0];
        assert_eq!(
            marked,
            [first, first + 1, first + 2],
            "{MARKED} packets in a row"
        );
        let quieter = heard.iter().position(|kind| *kind == kinds[2]).unwrap();
        assert!(heard[quieter..].iter().all(|kind| *kind == kinds[2]));

        // The decoder overlaps its units, so a packet is judged once the packet before it is of its kind.
        let rows = decoded(&heard);
        let settled = |kind: Kind| -> Vec<&Vec<f32>> {
            let places = (1..heard.len()).filter(|&at| heard[at] == kind && heard[at - 1] == kind);
            places.map(|at| &rows[at]).collect()
        };
        for kind in kinds {
            // Line k turns 2k + 1 times a packet of four units.
            let turns = usize::from(2 * kind.0 + 1);
            for row in settled(kind) {
                assert!(peak(row) > AUDIBLE, "{kind:?} peaks at {}", peak(row));
                let crossed = crossings(row);
                assert!(
                    crossed.abs_diff(2 * turns) <= CROSSINGS_SLACK,
                    "{kind:?}: {crossed}"
                );
            }
        }
        let loud = peak(settled(kinds[0])[0]);
        let quiet = peak(settled(kinds[2])[0]);
        assert!(
            (quiet / loud - HALF).abs() < RATIO_SLACK,
            "{quiet} against {loud}"
        );
    }
}
