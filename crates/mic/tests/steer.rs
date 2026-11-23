//! Support tests of the rate the reader asks its consumer for, through the chain's public API, with the test owning the clock; the chain has no user path yet.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the suites of this crate share one rig, and this one leaves some of its helpers unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::time::Duration;

    use pods_mic::{NODE_LATENCY_MS, RATE_HZ};

    use super::support::*;

    /// The longest gap measured between two packets.
    const WORST_GAP: Duration = Duration::from_millis(72);
    /// The steering never asks for more than 2000 parts per million either way.
    const MAX_STEER: f64 = 2000e-6;

    /// Runs pods that are `slow_ppm` slow against a consumer that follows the rate it is asked for, and gives the range, in ms, of the reader's distance from its target once `SETTLE` has passed.
    fn steered(slow_ppm: i32, span: Duration, gaps: bool) -> (Rig, [f64; 2]) {
        const SETTLE: Duration = Duration::from_secs(5);
        let pods_period = period().mul_f64(1.0 + f64::from(slow_ppm) / 1e6);
        let end = FIRST.saturating_add(span);
        let mut rig = Rig::new();
        let (mut packet, mut landed) = (0_u32, FIRST);
        // The frame of the run the next read opens on, once a read was found in the twin's stream, and the frames dropped from the front of that stream.
        let (mut next, mut dropped) = (None, 0_usize);
        let (mut low, mut high) = (f64::MAX, f64::MIN);
        let mut now = FIRST;
        while now < end {
            loop {
                let due = after(FIRST, pods_period, packet);
                // With `gaps`, every tenth packet lands the worst gap after the one before, and the packet due meanwhile lands with it.
                let lands = if gaps && packet > 0 && packet % 10 == 0 {
                    landed.saturating_add(WORST_GAP)
                } else {
                    due.max(landed)
                };
                if lands > now {
                    break;
                }
                rig.land(lands);
                (packet, landed) = (packet.saturating_add(1), lands);
            }
            let (heard, ratio) = rig.read(now, QUANTUM);
            next = match next {
                None => find(&rig.decoded, &heard).map(|at| at.saturating_add(QUANTUM)),
                Some(frame) => {
                    let from = frame.checked_sub(dropped).unwrap();
                    let twin = rig.decoded.get(from..from.saturating_add(QUANTUM));
                    assert!(
                        twin.map(bits) == Some(bits(&heard)),
                        "the read at {now:?} left the stream"
                    );
                    Some(frame.saturating_add(QUANTUM))
                }
            };
            assert!(
                next.is_some() || now < FIRST.saturating_add(SETTLE),
                "no read was found in the twin's stream"
            );
            if let Some(read) = next.filter(|_| now >= FIRST.saturating_add(SETTLE)) {
                // The frame that landed the stated latency before `now` when its packet was on time.
                let since = now.as_secs_f64() - FIRST.as_secs_f64();
                let played = since + pods_period.as_secs_f64() - ms(NODE_LATENCY_MS).as_secs_f64();
                let packet_frames = f64::from(u32::try_from(PACKET_FRAMES).unwrap());
                let target = played / pods_period.as_secs_f64() * packet_frames;
                let ahead_ms = (f64::from(u32::try_from(read).unwrap()) - target) * 1000.0
                    / f64::from(RATE_HZ);
                (low, high) = (low.min(ahead_ms), high.max(ahead_ms));
                let kept = dropped.saturating_add(PACKET_FRAMES);
                let keep = read.checked_sub(kept).unwrap();
                rig.decoded.drain(..keep);
                dropped = dropped.saturating_add(keep);
            }
            now = now.saturating_add(QUANTUM_TIME.div_f64(ratio));
        }
        (rig, [low, high])
    }

    fn ten_minutes(slow_ppm: i32) {
        let (rig, [low, high]) = steered(slow_ppm, Duration::from_secs(600), false);
        let (off, side) = (
            slow_ppm.unsigned_abs(),
            if slow_ppm < 0 { "fast" } else { "slow" },
        );
        let stood = format!("the reader stood {low:.3} to {high:.3} ms ahead of its target");
        writeln!(io::stderr(), "pods {off} ppm {side}: {stood}").unwrap();
        assert!(low > -2.0 && high < 2.0, "{low} to {high} ms");
        assert_eq!(Rig::count(&rig.counters.underruns), 0);
    }

    #[test]
    fn ten_minutes_with_the_pods_172_ppm_slow_never_underruns_and_never_jumps() {
        ten_minutes(172);
    }

    #[test]
    fn the_same_with_the_pods_300_ppm_fast() {
        ten_minutes(-300);
    }

    #[test]
    fn the_worst_measured_gap_is_played_whole() {
        let (rig, _) = steered(172, Duration::from_secs(60), true);
        let counted = [&rig.counters.underruns, &rig.counters.concealed];
        assert_eq!(counted.map(Rig::count), [0, 0]);
    }

    #[test]
    fn the_ratio_is_one_before_any_arrival_and_never_leaves_its_clamp() {
        // The distance is smoothed over two seconds of frames: one read 10 ms late asks for 20 parts per million more, not 2000.
        let mut late = Rig::new();
        for at in on_time(0..7) {
            late.land(at);
        }
        late.resync.request();
        let (_, on_target) = late.read(landing(6), QUANTUM);
        let (_, behind) = late.read(landing(6) + QUANTUM_TIME + ms(10), QUANTUM);
        assert!(unsteered(on_target), "{on_target}");
        assert!((behind - 1.0 - 20e-6).abs() < 1e-6, "{behind}");

        for slow in [true, false] {
            let mut rig = Rig::new();
            let (_, idle) = rig.read(Duration::ZERO, QUANTUM);
            assert!(unsteered(idle), "{idle}");
            // A consumer that ignores the ratio and takes its quanta as far from the pods' time as the steering can ask for.
            let drift = QUANTUM_TIME.mul_f64(MAX_STEER);
            let every = if slow {
                QUANTUM_TIME + drift
            } else {
                QUANTUM_TIME.checked_sub(drift).unwrap()
            };
            let mut lands = on_time(0..1000).into_iter().peekable();
            let mut ratios = Vec::new();
            let mut now = FIRST;
            while now < landing(990) {
                while let Some(at) = lands.next_if(|&at| at <= now) {
                    rig.land(at);
                }
                ratios.push(rig.read(now, QUANTUM).1);
                now += every;
            }
            // Read late, the reader falls behind and is asked to take more; read early, less. Each way the ask stops at the clamp.
            let low = ratios.iter().copied().fold(1.0, f64::min);
            let high = ratios.iter().copied().fold(1.0, f64::max);
            let (expected, clamp) = if slow {
                ((1.0, 1.0 + MAX_STEER), high)
            } else {
                ((1.0 - MAX_STEER, 1.0), low)
            };
            assert_eq!((low, high), expected, "slow: {slow}");
            // The drift ends in a jump, where the smoothing starts again from nothing: that read is asked for no other rate.
            let at_clamp = |ratio: &f64| ratio.to_bits() == clamp.to_bits();
            let clamped = ratios.iter().position(at_clamp).unwrap();
            assert!(
                ratios[clamped..].iter().any(|&ratio| unsteered(ratio)),
                "slow: {slow}"
            );
        }
    }
}
