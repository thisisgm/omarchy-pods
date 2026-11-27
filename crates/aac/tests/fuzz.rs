//! Robustness tests: random bytes, truncated, empty, oversized, and bit-flipped units never panic or emit non-finite samples.

#[cfg(test)]
mod tests {
    use pods_aac::{Decoder, EXPECTED_CONFIG, Error, MAX_UNIT_BYTES, SAMPLES_PER_UNIT};
    use proptest::prelude::*;

    const TONE_UNIT: [u8; 4] = [0xb4, 0x04, 0x42, 0x50];
    const SILENT_UNIT: [u8; 2] = [0x00, 0x00];

    fn xorshift64(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        #[test]
        fn arbitrary_bytes_never_panic_and_always_return_finite_frame_or_error(
            bytes in proptest::collection::vec(any::<u8>(), 0..300)
        ) {
            let mut decoder = Decoder::with_config(&EXPECTED_CONFIG).unwrap();
            if let Ok(samples) = decoder.decode(&bytes) {
                prop_assert_eq!(samples.len(), SAMPLES_PER_UNIT);
                prop_assert!(samples.iter().all(|s| s.is_finite()));
            }
        }

        #[test]
        fn arbitrary_config_other_than_expected_is_refused(
            config in proptest::collection::vec(any::<u8>(), 0..16)
        ) {
            prop_assume!(config.as_slice() != EXPECTED_CONFIG);
            prop_assert_eq!(Decoder::with_config(&config).unwrap_err(), Error::UnsupportedConfig);
        }
    }

    #[test]
    fn empty_oversized_and_seeded_mutations_never_panic() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.decode(&[]), Err(Error::EmptyUnit));
        assert_eq!(
            decoder.decode(&vec![0_u8; MAX_UNIT_BYTES + 1]),
            Err(Error::OversizedUnit(MAX_UNIT_BYTES + 1))
        );
        assert_ne!(
            decoder.decode(&vec![0_u8; MAX_UNIT_BYTES]),
            Err(Error::OversizedUnit(MAX_UNIT_BYTES))
        );

        let mut rng = 0x5eed_aac0_0001_u64;
        let mut buf = [0_u8; 128];
        // Units longer than the silent one, so the empty and one-byte cuts cannot satisfy either count.
        let mut frames_past_silence = 0_usize;
        let mut refusals_past_silence = 0_usize;
        for case in 0..50_000_usize {
            let slice: &[u8] = match case % 3 {
                0 => {
                    let len = usize::try_from(xorshift64(&mut rng) % 128).unwrap();
                    for b in &mut buf[..len] {
                        *b = u8::try_from(xorshift64(&mut rng) & 0xff).unwrap();
                    }
                    &buf[..len]
                }
                1 => {
                    buf[..TONE_UNIT.len()].copy_from_slice(&TONE_UNIT);
                    let total_bits = u64::try_from(TONE_UNIT.len() * 8).unwrap();
                    let bit = usize::try_from(xorshift64(&mut rng) % total_bits).unwrap();
                    buf[bit / 8] ^= 1_u8 << (bit % 8);
                    &buf[..TONE_UNIT.len()]
                }
                _ => {
                    buf[..SILENT_UNIT.len()].copy_from_slice(&SILENT_UNIT);
                    let len = usize::try_from(xorshift64(&mut rng) % 3).unwrap();
                    &buf[..len]
                }
            };
            let past_silence = slice.len() > SILENT_UNIT.len();
            match decoder.decode(slice) {
                Ok(samples) => {
                    assert_eq!(samples.len(), SAMPLES_PER_UNIT);
                    assert!(samples.iter().all(|s| s.is_finite()));
                    frames_past_silence += usize::from(past_silence);
                }
                Err(_) => refusals_past_silence += usize::from(past_silence),
            }
        }
        assert!(frames_past_silence > 0, "no mutated unit decoded");
        assert!(refusals_past_silence > 0, "no mutated unit was refused");
    }
}
