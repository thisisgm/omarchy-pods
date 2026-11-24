//! Support test: the chain's writer on one thread and its reader on another, bounded by a count of packets, the writer paced by the reader's reads; it reads no clock and waits on no time.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the suites of this crate share one rig, and this one leaves some of its helpers unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::thread::{self, JoinHandle};

    use pods_mic::{Chain, Counters, NODE_LATENCY_MS, STAMP_STEP, chain};
    use pods_samples::uplink::tone_unit;

    use super::support::{
        FADE, HELD, PACKET_FRAMES, SHAPE_SLACK, UNITS_PER_PACKET, frames_in, landing, ms, rise,
    };

    const PACKETS: u32 = 1000;
    /// A thirty-second less than the ring hands out: with the stated latency before it, each read opens on frames the writer is about to write over.
    const LONG: usize = HELD - HELD / 32;
    /// The packets the writer may land past the one a read is timed by: it writes over the start of that read and never reaches its end.
    const AHEAD: u32 = 3;

    /// The writer's thread and the gate it lands under; dropping it opens the gate and waits for the thread, on every way out of the test.
    struct Writer {
        thread: Option<JoinHandle<()>>,
        allowed: Arc<AtomicU32>,
    }

    impl Drop for Writer {
        fn drop(&mut self) {
            self.allowed.store(u32::MAX, Ordering::Release);
            drop(self.thread.take().map(JoinHandle::join));
        }
    }

    #[test]
    fn a_reader_on_another_thread_gets_what_the_writer_wrote_in_order() {
        /// Lands one packet on time: its units of the cycle.
        fn land(chain: &mut Chain, units: &[Vec<u8>], packet: u32) {
            let sent = packet * UNITS_PER_PACKET..(packet + 1) * UNITS_PER_PACKET;
            let cycle = |unit: u32| &units[usize::try_from(unit).unwrap() % units.len()];
            let stamped = sent.map(|unit| (unit * STAMP_STEP, cycle(unit).as_slice()));
            chain.sdu(landing(packet), stamped);
        }
        let units = [(8, 172), (15, 177), (22, 182), (13, 174), (20, 179)]
            .map(|(line, level)| tone_unit(line, level));

        // What the ring holds of the run, as a reader in step with the writer on one thread is handed it.
        let (mut alone, mut in_step, resync) = chain(Arc::default()).unwrap();
        resync.request();
        let mut written = vec![0.0; PACKET_FRAMES * usize::try_from(PACKETS).unwrap()];
        for (packet, frames) in (0..PACKETS).zip(written.chunks_mut(PACKET_FRAMES)) {
            land(&mut alone, &units, packet);
            in_step.read(landing(packet) + ms(NODE_LATENCY_MS), frames);
        }
        // Past the fade in of its first packet no sample of the run is zero, so a zero a read holds was never the writer's.
        assert!(written[PACKET_FRAMES..].iter().all(|&wrote| wrote != 0.0));

        let counters = Arc::new(Counters::default());
        let (mut chain, mut reader, resync) = chain(Arc::clone(&counters)).unwrap();
        let (landed, allowed) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
        let thread = thread::spawn({
            let (landed, allowed) = (Arc::clone(&landed), Arc::clone(&allowed));
            move || {
                for packet in 0..PACKETS {
                    // The writer waits on the reader, never on time.
                    while allowed.load(Ordering::Acquire) <= packet {
                        thread::yield_now();
                    }
                    land(&mut chain, &units, packet);
                    landed.store(packet + 1, Ordering::Release);
                }
            }
        });
        let mut writer = Writer {
            thread: Some(thread),
            allowed,
        };

        // A read ends the stated latency behind the audio that landed, and the writer comes no more than `AHEAD` packets past that: the frames it cannot reach end every read whole, less the fade in that opens them.
        let latency = frames_in(ms(NODE_LATENCY_MS));
        let sure = HELD - usize::try_from(AHEAD).unwrap() * PACKET_FRAMES - latency - FADE;
        let (mut heard, mut raced) = (vec![0.0_f32; LONG], 0_u32);
        loop {
            let over = writer.thread.as_ref().is_none_or(JoinHandle::is_finished);
            // A recorder's clock is never ahead of the audio that landed: a read's time is that of the last packet seen.
            let packets = landed.load(Ordering::Acquire);
            writer.allowed.store(packets + AHEAD, Ordering::Release);
            let landed_frames = usize::try_from(packets).unwrap() * PACKET_FRAMES;
            let first = landed_frames.checked_sub(LONG + latency);
            if let Some(first) = first.filter(|&first| first >= PACKET_FRAMES) {
                resync.request();
                // The two loads bracket the read alone: a packet counted here finished landing while it ran.
                let before = landed.load(Ordering::Acquire);
                reader.read(landing(packets - 1), &mut heard);
                raced += u32::from(landed.load(Ordering::Acquire) != before);
                let (mut faded, mut exact) = (0, 0_usize);
                for (&heard, &wrote) in heard.iter().zip(&written[first..]) {
                    // Silence is a frame written over, or the first of a fade in; the count is of the run that ends the read.
                    if heard == 0.0 {
                        (faded, exact) = (1, 0);
                        continue;
                    }
                    let same = if faded < FADE {
                        (heard - wrote * rise(faded)).abs() <= SHAPE_SLACK
                    } else {
                        heard.to_bits() == wrote.to_bits()
                    };
                    assert!(same, "{heard} for {wrote}, {faded} into a fade");
                    exact += usize::from(faded >= FADE);
                    faded += 1;
                }
                assert!(
                    exact >= sure,
                    "the last {exact} samples of the read at packet {packets} came out as written"
                );
            }
            if over {
                break;
            }
        }
        let ended = writer.thread.take().map(JoinHandle::join);
        assert!(matches!(ended, Some(Ok(()))), "the writer panicked");
        assert!(raced > 0, "the writer landed no packet under any read");
        let counted = [
            &counters.underruns,
            &counters.concealed,
            &counters.decode_errors,
            &counters.decoder_resets,
        ]
        .map(|counter| counter.load(Ordering::Relaxed));
        assert_eq!(counted, [0; 4]);
    }
}
