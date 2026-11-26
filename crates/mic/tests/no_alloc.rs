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
    fn a_quiet_cut_never_allocates() {
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
        let kept = loud.reader.read(now + READ_EVERY + MISSED, &mut heard);
        assert!(!unsteered(kept), "the tone was cut, ratio {kept}");
    }
}
