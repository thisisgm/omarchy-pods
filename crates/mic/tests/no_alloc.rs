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

    use super::support::*;

    /// A quarter of a millisecond a read.
    const FRAMES: usize = 16;
    const READ_EVERY: Duration = Duration::from_micros(250);
    /// Eight packets: all the ring holds without overwriting, 240 ms of audio.
    const PACKETS: u32 = 8;
    /// The reads before the consumer comes late, the reads from there until a recorder links, and the reads after: 2.5 s in all.
    const STRETCHES: [u32; 3] = [200, 200, 9600];
    /// How late the consumer comes once, which is further than a reader may stand from its target.
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
        // Every read before the link held audio, on both sides of the realign; the reads after it met audio and then the end of it, one dry stretch.
        assert_eq!(loud[..2], STRETCHES[..2]);
        assert!(loud[2] > 0 && loud[2] < STRETCHES[2], "{loud:?}");
        assert_eq!(Rig::count(&rig.counters.underruns), 1);
        // The counter does count in this binary: one vector is one allocation.
        let control = allocation_counter::measure(|| drop(black_box(vec![0_u8; FRAMES])));
        assert_eq!(control.count_total, 1);
    }
}
