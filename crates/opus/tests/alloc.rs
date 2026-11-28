//! Zero-allocation proof for `pods-opus` decode and conceal over 1,000+ frames under a counting allocator.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::hint::black_box;

    use pods_opus::{Decoder, OutputRate};
    use pods_samples::opus::tone;

    #[test]
    fn decode_and_conceal_make_zero_allocations_over_1000_frames() {
        let frames: Vec<&[u8]> = tone().collect();
        for rate in [OutputRate::Hz48000, OutputRate::Hz16000] {
            let mut dec = Decoder::new(rate);
            let expected = rate.frame_samples();

            let decode_stats = allocation_counter::measure(|| {
                for i in 0..1_000_usize {
                    let pcm = dec.decode(frames[i % frames.len()]).unwrap();
                    assert_eq!(pcm.len(), expected);
                    black_box(pcm[0]);
                }
            });
            assert_eq!(decode_stats.count_total, 0, "decode allocated at {rate:?}");

            let conceal_stats = allocation_counter::measure(|| {
                for _ in 0..1_000_usize {
                    let pcm = dec.conceal();
                    assert_eq!(pcm.len(), expected);
                    black_box(pcm[0]);
                }
            });
            assert_eq!(
                conceal_stats.count_total, 0,
                "conceal allocated at {rate:?}"
            );

            let err_stats = allocation_counter::measure(|| {
                assert!(dec.decode(&[]).is_err());
                assert!(dec.decode(&[0x5d, 0x00]).is_err());
                assert!(dec.decode(&[0xb8; 95]).is_err());
            });
            assert_eq!(
                err_stats.count_total, 0,
                "refused decode allocated at {rate:?}"
            );
        }

        // Positive control: prove the counting allocator is active in this binary.
        let control = allocation_counter::measure(|| drop(black_box(vec![0_u8; 960])));
        assert_eq!(control.count_total, 1);
    }
}
