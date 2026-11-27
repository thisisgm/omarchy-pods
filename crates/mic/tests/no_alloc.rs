//! Support test: the real time half of the chain under a counting allocator, which stands in for the system's in this test binary alone.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the suites of this crate share one rig, and this one leaves some of its helpers unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::hint::black_box;
    use std::time::Duration;

    use pods_samples::uplink::silent_unit;

    use super::support::*;

    /// A quarter of a millisecond a read.
    const FRAMES: usize = 16;
    const READ_EVERY: Duration = Duration::from_micros(250);
    /// Eight packets, 240 ms of audio: what the reads before the link take, and some for those after it.
    const PACKETS: u32 = 8;
    /// The reads before the consumer comes late, the reads from there until a recorder links, and the reads after: 2.5 s in all.
    const STRETCHES: [u32; 3] = [200, 200, 9600];
    /// How late the consumer comes once, past the realign bound.
    const MISSED: Duration = Duration::from_millis(25);

    #[test]
    fn the_reader_never_allocates() {
        let mut rig = Rig::new();
        for at in on_time(0..PACKETS) {
            rig.land(at);
        }
        let mut heard = [0.0_f32; FRAMES];
        let mut loud = [0_u32; 3];
        let mut now = landing(4);
        let reading = allocation_counter::measure(|| {
            for (stretch, reads) in STRETCHES.into_iter().enumerate() {
                match stretch {
                    1 => now += MISSED,
                    2 => rig.resync.request(),
                    _ => {}
                }
                for _ in 0..reads {
                    black_box(rig.reader.read(now, &mut heard));
                    loud[stretch] += u32::from(heard.iter().any(|&sample| sample != 0.0));
                    now += READ_EVERY;
                }
            }
        });
        assert_eq!(reading.count_total, 0);
        // Every read before the link held audio despite the stall; the reads after it met audio and then one dry stretch.
        assert_eq!(loud[..2], STRETCHES[..2]);
        assert!(loud[2] > 0 && loud[2] < STRETCHES[2], "{loud:?}");
        assert_eq!(Rig::count(&rig.counters.underruns), 1);
        // The counter does count in this binary: one vector is one allocation.
        let control = allocation_counter::measure(|| drop(black_box(vec![0_u8; FRAMES])));
        assert_eq!(control.count_total, 1);
    }

    #[test]
    fn quiet_cuts_and_speech_catch_up_never_allocate() {
        let mut rig = Rig::new();
        let silence = vec![silent_unit(); usize::try_from(UNITS_PER_PACKET).unwrap()];
        for at in on_time(0..PACKETS) {
            rig.send(at, &silence);
        }
        let mut heard = [0.0; FRAMES];
        let now = landing(4);
        rig.resync.request();
        rig.reader.read(now, &mut heard);
        let mut ratio = 0.0;
        let measured = allocation_counter::measure(|| {
            ratio = black_box(rig.reader.read(now + READ_EVERY + MISSED, &mut heard));
        });
        assert_eq!(measured.count_total, 0);
        assert!(unsteered(ratio), "the reader kept the gap, ratio {ratio}");

        // Over the tone the same late read keeps the gap, so only the quiet cut returns the nominal rate.
        let mut loud = Rig::new();
        for at in on_time(0..PACKETS) {
            loud.land(at);
        }
        loud.resync.request();
        loud.reader.read(now, &mut heard);
        let mut catch_up = 0.0;
        let measured = allocation_counter::measure(|| {
            let late = now + READ_EVERY + MISSED;
            catch_up = black_box(loud.reader.read_for_converter(late, &mut heard, FRAMES));
            black_box(
                loud.reader
                    .read_for_converter(late + READ_EVERY, &mut heard, FRAMES),
            );
        });
        assert_eq!(measured.count_total, 0);
        assert!(
            catch_up > 1.0 + MAX_STEER && catch_up <= 1.05,
            "ratio {catch_up} stayed at the drift clamp"
        );
    }

    #[test]
    fn the_uplink_decoder_never_allocates_across_1000_units() {
        use pods_mic::decode::{Decoded, Decoder};
        use pods_samples::uplink::tone_unit;

        let mut decoder = Decoder::new().unwrap();
        let tone = tone_unit(10, 180);
        let silent = silent_unit();
        assert!(matches!(decoder.unit(&tone), Decoded::Samples(_)));
        assert!(matches!(decoder.unit(&silent), Decoded::Samples(_)));

        let mut ok_units = 0_usize;
        let measured = allocation_counter::measure(|| {
            for i in 0..1000_usize {
                let unit = if i % 2 == 0 {
                    tone.as_slice()
                } else {
                    silent.as_slice()
                };
                if matches!(black_box(decoder.unit(unit)), Decoded::Samples(_)) {
                    ok_units += 1;
                }
            }
        });
        assert_eq!(ok_units, 1000);
        assert_eq!(measured.count_total, 0);
    }

    #[test]
    fn the_resampler_never_allocates() {
        use pods_mic::resample::Resampler;

        const LOW_HZ: u32 = 48_000;
        const HIGH_HZ: u32 = 64_000;
        const BLOCK: usize = 480;
        const MAX_OUT: usize = 640;

        let input: [f32; BLOCK] = std::array::from_fn(|i| {
            let phase = f64::from(u32::try_from(i).unwrap()) * 0.1;
            #[expect(
                clippy::as_conversions,
                clippy::cast_possible_truncation,
                reason = "test sine sample inside full scale"
            )]
            let s = phase.sin() as f32;
            s
        });
        let mut out = Vec::with_capacity(MAX_OUT);
        let mut total_out = 0_usize;

        let measured = allocation_counter::measure(|| {
            for (from_hz, to_hz) in [(LOW_HZ, HIGH_HZ), (HIGH_HZ, LOW_HZ)] {
                let mut resampler = black_box(Resampler::new(from_hz, to_hz).unwrap());
                for _ in 0..8 {
                    out.clear();
                    resampler.push(black_box(&input), &mut out);
                    total_out += black_box(out.len());
                }
                resampler.reset();
                out.clear();
                resampler.push(black_box(&input), &mut out);
                total_out += black_box(out.len());
            }
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(total_out, 9 * (640 + 360));
    }
}
