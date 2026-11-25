//! Support tests: the fake pods' dictation service over socket pairs; a frame's time is held to the instant the fake heard the mode and an answer's to the test's own clock; no user path, it is a test crate.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod support;

#[cfg(test)]
#[path = "support/arbitrary.rs"]
mod arbitrary;

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use pods_fake::doap::{BURST_PER_NOTIFICATION, Doap, FRAME, GARBLED_TOC, MODE_SETTLE};
    use pods_fake::{Deadline, Pods, Profile, pair};
    use pods_mic::decode::Decoded;
    use pods_mic::opus::{Decoder, FRAME_SAMPLES, MAX_OPUS_FRAME_BYTES};
    use pods_samples::doap::{self, Handles, REASON_HOST, REASON_UNASKED, host};
    use pods_samples::opus::{mark, tone};
    use pods_samples::{DOAP_MODE, setting};
    use pods_sys::l2cap::Channel;
    use proptest::prelude::*;

    use super::arbitrary;
    use super::support::{
        REAL_TIME_SLACK, RECV_TIMEOUT, ask, assert_quiet, open, opening, recv, recv_many,
    };

    const TABLE: Handles = Handles::OBSERVED;
    /// Opus configuration 23 as one mono frame: CELT only, wideband, 20 ms.
    const TOC_CELT_WIDEBAND_20MS_MONO: u8 = 0xb8;
    /// How soon the pods refuse a start, and how soon they answer a stop.
    const REFUSED_WITHIN: Duration = Duration::from_millis(20);
    const ENDED_WITHIN: Duration = Duration::from_millis(30);
    /// The frames of the mark, and how long before the start it ends.
    const MARKED: u32 = 5;
    const MARK_LEAD: Duration = Duration::from_millis(300);
    /// The frames of each fixture, which is one second.
    const FIXTURE_FRAMES: usize = 50;
    /// A burst small enough that the microphone fills it in a fifth of a second, and the frames of each of its notifications, three at most.
    const SHORT_BURST: u32 = 8;
    const SHORT_SPLIT: [u16; 3] = [3, 3, 2];
    /// The frames the microphone hears past those a test reads back, so every one of them was heard.
    const SPARE: u32 = 2;
    /// The burst of the test that spoils frames, which goes in one notification.
    const SPOILED_BURST: u32 = 4;
    /// The frames the pods send with no start heard.
    const UNASKED: u32 = 2;
    /// The frames taken after the burst, each held to its time.
    const STEADY: u16 = 5;
    /// The opcode of the MTU request, which stands for any request of the pods' own.
    const MTU_REQUEST: u8 = 0x02;
    /// The gap a test stalls the stream for, and the notifications read across it.
    const STALLED: Duration = Duration::from_millis(120);
    const ACROSS_THE_STALL: u16 = 8;
    /// The most packets read for the end of a stream: the frames that may still be on their way, and the end.
    const TAIL: usize = 10;

    /// The pods on a control channel and an attribute channel; the drop ends every serving thread and waits for it on every way out.
    struct Served {
        pods: Pods,
        doap: Doap,
        control: Channel,
        att: Channel,
    }

    impl Served {
        fn new() -> Self {
            let pods = Pods::new(Profile::default());
            let (control, att) = connect(&pods);
            Self {
                doap: pods.doap(),
                pods,
                control,
                att,
            }
        }

        /// Sets the mode and returns when the fake heard it, which is where its microphone counts from.
        fn mode(&self) -> Instant {
            let mode = setting(DOAP_MODE, 1);
            ask(&self.control, &mode);
            assert_eq!(recv(&self.control), mode);
            self.pods.wait_heard(&mode, 1, RECV_TIMEOUT).unwrap()[0].at
        }

        /// Lets the microphone hear for `time`, held to the fake's own wait: no channel may end meanwhile.
        fn hear_for(&self, time: Duration) {
            let ended = self.pods.wait_closed(1, time);
            assert!(
                matches!(ended, Err(Deadline { waited }) if waited >= time),
                "a channel ended: {ended:?}"
            );
        }

        /// Sends a start and takes the score; returns the instants around the fake hearing it.
        fn start(&self) -> (Instant, Instant) {
            let asked = Instant::now();
            ask(&self.att, &host::start(&TABLE));
            assert_eq!(recv(&self.att), doap::score(&TABLE));
            (asked, Instant::now())
        }

        /// The pods drop the link and the host connects again.
        fn reconnect(&mut self) {
            self.pods.hang_up();
            self.pods.wait_hung_up(RECV_TIMEOUT).unwrap();
            (self.control, self.att) = connect(&self.pods);
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

    /// Opens both channels as a host does: the opening on the control channel, then the four subscriptions and the codec.
    fn connect(pods: &Pods) -> (Channel, Channel) {
        let control = open(pods);
        opening(&control);
        let (att, end) = pair();
        att.set_recv_timeout(RECV_TIMEOUT).unwrap();
        pods.serve_att(end);
        for descriptor in TABLE.cccds {
            ask(&att, &host::subscribe(descriptor));
            assert_eq!(recv(&att), doap::WRITTEN);
        }
        ask(&att, &host::select_codec(&TABLE));
        (control, att)
    }

    /// The frame the microphone codes at `place` since the mode was set: the fixture's second, over and over.
    fn coded(fixture: &[&'static [u8]], place: u16) -> &'static [u8] {
        fixture[usize::from(place).checked_rem(fixture.len()).unwrap()]
    }

    /// One notification of the tone: `frames` frames from `counter` on, which is also their place.
    fn tones(packet_counter: u8, counter: u16, frames: u16) -> Vec<u8> {
        let fixture: Vec<&[u8]> = tone().collect();
        let held: Vec<(u16, &[u8])> = (counter..counter.checked_add(frames).unwrap())
            .map(|counter| (counter, coded(&fixture, counter)))
            .collect();
        doap::audio(&TABLE, packet_counter, &held)
    }

    /// The bytes of an audio notification before its first frame, and those of a frame before its counter: the flag and the level.
    const AUDIO_PREFIX: usize = 8;
    const FRAME_PREFIX: usize = 2;

    /// The counter of the first frame of an audio notification.
    // Sample input: `1b 0d 00 01 00 2a 08 00 80 d5 04 01 03 b8 10 11`, whose first frame has the counter 0x0104.
    fn first_counter(pdu: &[u8]) -> u16 {
        let counter = &pdu[AUDIO_PREFIX..][FRAME_PREFIX..][..size_of::<u16>()];
        u16::from_le_bytes(counter.try_into().unwrap())
    }

    /// When the microphone that started at `since` has heard the frame at `place`.
    fn heard_at(since: Instant, place: u16) -> Instant {
        let after = FRAME.saturating_mul(u32::from(place).saturating_add(1));
        since.checked_add(after).unwrap()
    }

    #[test]
    fn a_start_without_the_mode_is_refused_with_reason_6() {
        let served = Served::new();
        let asked = Instant::now();
        ask(&served.att, &host::start(&TABLE));
        assert_eq!(recv(&served.att), doap::stopped(&TABLE, REASON_UNASKED));
        let waited = asked.elapsed();
        assert!(
            waited <= REFUSED_WITHIN + REAL_TIME_SLACK,
            "refused after {waited:?}"
        );
        assert_quiet(&served.att);
        assert_eq!(served.doap.starts(), 1);
        assert!(!served.doap.mode_seen() && !served.doap.streaming());
    }

    #[test]
    fn the_start_brings_the_score_then_the_burst_then_one_frame_every_20_ms() {
        let served = Served::new();
        served.doap.burst(SHORT_BURST, u32::from(SHORT_SPLIT[0]));
        let since = served.mode();
        served.hear_for(FRAME * (SHORT_BURST + SPARE));
        let (asked, answered) = served.start();
        assert!(served.doap.streaming());

        let burst = recv_many(&served.att, SHORT_SPLIT.len());
        let first = first_counter(&burst[0]);
        let mut after = first;
        for ((packet, frames), sent) in (0..).zip(SHORT_SPLIT).zip(&burst) {
            assert_eq!(*sent, tones(packet, after, frames), "notification {packet}");
            after += frames;
        }
        assert_eq!(after - first, u16::try_from(SHORT_BURST).unwrap());
        // The burst ends with the last frame heard before the start: the next one was not heard when the start was asked for.
        assert!(
            heard_at(since, after - 1) <= answered,
            "the burst holds a frame not heard yet"
        );
        assert!(
            asked < heard_at(since, after),
            "the burst leaves out a frame heard before the start"
        );

        let burst_packets = u8::try_from(SHORT_SPLIT.len()).unwrap();
        for (packet, counter) in (burst_packets..).zip(after..after + STEADY) {
            let frame = recv(&served.att);
            let at = Instant::now();
            assert_eq!(frame, tones(packet, counter, 1), "frame {counter}");
            let due = heard_at(since, counter);
            assert!(
                due <= at && at.saturating_duration_since(due) <= REAL_TIME_SLACK,
                "frame {counter} came {:?} after its time and {:?} before it",
                at.saturating_duration_since(due),
                due.saturating_duration_since(at)
            );
        }

        let asked = Instant::now();
        ask(&served.att, &host::stop(&TABLE, REASON_HOST));
        // Frames heard before the stop may still be on their way; the end follows them.
        let end = doap::end(&TABLE);
        assert!((0..TAIL).any(|_| recv(&served.att) == end), "no end event");
        let waited = asked.elapsed();
        assert!(
            waited <= ENDED_WITHIN + REAL_TIME_SLACK,
            "ended after {waited:?}"
        );
        assert_quiet(&served.att);
        assert_eq!(served.doap.stops(), [REASON_HOST]);
        assert!(!served.doap.streaming());
    }

    #[test]
    fn the_burst_holds_what_was_heard_before_the_start() {
        let served = Served::new();
        served.doap.mark(MARKED);
        served.mode();
        served.hear_for(FRAME * MARKED + MARK_LEAD);
        served.start();

        let marks: Vec<&[u8]> = mark().collect();
        let marked: Vec<(u16, &[u8])> = (0..u16::try_from(MARKED).unwrap())
            .map(|counter| (counter, coded(&marks, counter)))
            .collect();
        assert_eq!(MARKED, BURST_PER_NOTIFICATION);
        assert_eq!(recv(&served.att), doap::audio(&TABLE, 0, &marked));
        // The mark ended at least 300 ms before the start, so the burst goes on with the tone heard in that time.
        let marked = u16::try_from(MARKED).unwrap();
        let lead = u16::try_from(MARK_LEAD.as_millis() / FRAME.as_millis()).unwrap();
        let before = marked + lead;
        for (packet, counter) in (1..).zip((marked..before).step_by(usize::from(marked))) {
            assert_eq!(
                recv(&served.att),
                tones(packet, counter, marked),
                "from {counter}"
            );
        }
        // Whatever was heard after that, the burst leaves none of it out: the next notification goes on at the next counter, and so does the one behind it.
        let packet = u8::try_from(before / marked).unwrap();
        let next = recv(&served.att);
        let held = (1..=marked).find(|frames| next == tones(packet, before, *frames));
        let Some(held) = held else {
            panic!("the notification behind the lead does not go on at {before}: {next:02x?}");
        };
        let behind = before + held;
        assert_eq!(first_counter(&recv(&served.att)), behind);
    }

    #[test]
    fn a_reconnect_forgets_the_mode() {
        let mut served = Served::new();
        served.mode();
        assert!(served.doap.mode_seen());
        served.hear_for(MODE_SETTLE);
        served.start();
        served.reconnect();
        assert!(!served.doap.mode_seen() && !served.doap.streaming());
        ask(&served.att, &host::start(&TABLE));
        assert_eq!(recv(&served.att), doap::stopped(&TABLE, REASON_UNASKED));
        assert_quiet(&served.att);
        assert_eq!(served.doap.starts(), 2);
    }

    #[test]
    fn every_fixture_frame_is_celt_wideband_20ms_mono_and_at_most_94_bytes() {
        for fixture in [tone().collect::<Vec<_>>(), mark().collect()] {
            assert_eq!(fixture.len(), FIXTURE_FRAMES);
            let mut decoder = Decoder::new().unwrap();
            for frame in fixture {
                assert_eq!(frame[0], TOC_CELT_WIDEBAND_20MS_MONO);
                assert!(frame.len() <= MAX_OPUS_FRAME_BYTES, "{} bytes", frame.len());
                let heard = decoder.frame(frame);
                assert!(
                    matches!(heard, Decoded::Samples(samples) if samples.len() == FRAME_SAMPLES),
                    "{heard:?}"
                );
            }
        }
    }

    #[test]
    fn the_counter_wraps_from_65535_to_0() {
        let served = Served::new();
        served.doap.start_counter(u16::MAX - 1);
        served.mode();
        served.hear_for(FRAME * (BURST_PER_NOTIFICATION + SPARE));
        served.start();
        let fixture: Vec<&[u8]> = tone().collect();
        let counters = [u16::MAX - 1, u16::MAX, 0, 1, 2];
        let across: Vec<(u16, &[u8])> = (0..)
            .zip(counters)
            .map(|(place, counter)| (counter, coded(&fixture, place)))
            .collect();
        assert_eq!(recv(&served.att), doap::audio(&TABLE, 0, &across));
    }

    #[test]
    fn a_start_is_refused_or_held_when_a_test_asks_and_the_pods_end_the_stream_themselves() {
        let served = Served::new();
        served.doap.burst(0, 1);
        served.mode();
        served.hear_for(MODE_SETTLE);
        served.doap.refuse_starts(1);
        ask(&served.att, &host::start(&TABLE));
        assert_eq!(recv(&served.att), doap::stopped(&TABLE, REASON_UNASKED));

        served.doap.hold_start(true);
        ask(&served.att, &host::start(&TABLE));
        assert_quiet(&served.att);
        assert!(!served.doap.streaming());
        served.doap.release_start();
        assert_eq!(recv(&served.att), doap::score(&TABLE));
        assert!(served.doap.streaming());

        served.doap.end_unasked();
        let end = doap::end(&TABLE);
        assert!((0..TAIL).any(|_| recv(&served.att) == end), "no end event");
        assert_quiet(&served.att);
        assert!(!served.doap.streaming() && served.doap.stops().is_empty());
        assert_eq!(served.doap.starts(), 2);
    }

    #[test]
    fn the_pods_send_audio_a_request_and_an_indication_nobody_asked_for() {
        let served = Served::new();
        served.mode();
        served.hear_for(FRAME * (UNASKED + SPARE));
        served.doap.audio_unasked(UNASKED);
        let unasked = recv_many(&served.att, usize::try_from(UNASKED).unwrap());
        let first = first_counter(&unasked[0]);
        assert_eq!(unasked, [tones(0, first, 1), tones(1, first + 1, 1)]);

        served.doap.send_request(MTU_REQUEST);
        assert_eq!(recv(&served.att), doap::request(MTU_REQUEST));
        ask(&served.att, &host::refusal(MTU_REQUEST));
        served.doap.indicate();
        assert_eq!(recv(&served.att), doap::UNASKED_INDICATION);
        ask(&served.att, &host::CONFIRMED);
        // The read behind them is answered once the fake has taken both.
        ask(&served.att, &host::read(TABLE.supported_codecs));
        assert_eq!(recv(&served.att), doap::CODECS);
        assert_eq!(served.doap.answered(), (1, 1));
        assert_eq!(served.doap.starts(), 0);
        assert_quiet(&served.att);
    }

    #[test]
    fn a_test_loses_garbles_repeats_and_stalls_frames() {
        let served = Served::new();
        served.doap.burst(SPOILED_BURST, SPOILED_BURST);
        served.doap.repeat_in_burst(1);
        served.doap.lose(1);
        served.doap.garble(1);
        served.mode();
        served.hear_for(FRAME * (SPOILED_BURST + SPARE));
        served.start();

        // Of the burst's four frames the first is lost and the second goes out twice, garbled the first time.
        let burst = recv_many(&served.att, 2);
        let twice = first_counter(&burst[0]);
        let fixture: Vec<&[u8]> = tone().collect();
        let mut garbled = coded(&fixture, twice).to_vec();
        garbled[0] = GARBLED_TOC;
        let kept = [
            (twice, garbled.as_slice()),
            (twice, coded(&fixture, twice)),
            (twice + 1, coded(&fixture, twice + 1)),
        ];
        let expected = [doap::audio(&TABLE, 0, &kept), tones(1, twice + 2, 1)];
        assert_eq!(burst, expected);

        served.doap.stall(STALLED);
        // The stall holds one frame and loses none: every counter still follows the one before it.
        let after = twice + 3;
        for (packet, counter) in (2..).zip(after..after + ACROSS_THE_STALL) {
            assert_eq!(
                recv(&served.att),
                tones(packet, counter, 1),
                "frame {counter}"
            );
        }
        // Held to the fake's clock, since a host that reads late sees a shorter gap: the stall held its frame, and not past its time.
        let gap = served.doap.largest_gap().unwrap();
        assert!(
            STALLED <= gap && gap <= STALLED + REAL_TIME_SLACK,
            "the longest gap was {gap:?}"
        );
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_on_the_attribute_channel_never_stop_the_fake(sdus in arbitrary::sdus()) {
            let served = Served::new();
            for sdu in &sdus {
                ask(&served.att, sdu);
            }
            ask(&served.att, &host::read(TABLE.supported_codecs));
            // A random SDU that is a request or a start is answered first, with one packet at most.
            let most_replies = sdus.len().saturating_add(1);
            let answered = (0..most_replies).any(|_| recv(&served.att) == doap::CODECS);
            prop_assert!(answered);
        }
    }
}
