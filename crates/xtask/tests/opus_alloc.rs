//! The decode call of the Opus decoder candidate under a counting allocator, which stands in for the system's in this test binary alone.

#[cfg(test)]
#[path = "../src/probe/xorshift.rs"]
mod xorshift;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::hint::black_box;

    use opuscule::{Channels, Decoder, SampleRate};
    use pods_proto::dictation::TOC_CELT_WIDEBAND_20MS_MONO;

    use super::xorshift::next_random;

    const SEED: u64 = 0x5eed_0019;
    /// The samples of one 20 ms frame at 48 kHz, and of the longest packet Opus allows.
    const FRAME: usize = 960;
    const LONGEST: usize = 5760;
    /// The coded bytes after the table of contents byte of the frame this test decodes.
    const CODED_BYTES: usize = 80;

    #[test]
    fn the_decode_call_does_not_allocate() {
        let mut random = SEED;
        let mut frame = vec![TOC_CELT_WIDEBAND_20MS_MONO];
        frame.extend((0..CODED_BYTES).map(|_| next_random(&mut random).to_le_bytes()[0]));
        let mut decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono);
        let mut pcm = vec![0.0_f32; LONGEST];
        // The first call is the warm up: whatever a decoder sets up once is not what a stream pays a frame.
        assert_eq!(decoder.decode(Some(&frame), &mut pcm, false), Ok(FRAME));

        let decoding = allocation_counter::measure(|| {
            let heard = decoder.decode(Some(&frame), &mut pcm, false);
            let concealed = decoder.decode(None, &mut pcm[..FRAME], false);
            assert_eq!((heard, concealed), (Ok(FRAME), Ok(FRAME)));
        });
        assert_eq!(decoding.count_total, 0);
        // The counter does count in this binary: one vector is one allocation.
        let control = allocation_counter::measure(|| drop(black_box(vec![0_u8; FRAME])));
        assert_eq!(control.count_total, 1);
    }
}
