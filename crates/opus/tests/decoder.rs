//! Integration tests for the clean-room `pods-opus` CELT wideband 20 ms mono decoder.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::hint::black_box;
    use std::time::Instant;

    use pods_opus::{
        Decoder, Error, MAX_PACKET_BYTES, OutputRate, SAMPLES_16K, SAMPLES_48K,
        TOC_CELT_WIDEBAND_20MS_MONO,
    };
    use pods_samples::opus::{mark, tone};
    use proptest::prelude::*;

    fn max_step(samples: &[f32]) -> f32 {
        samples
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0_f32, f32::max)
    }

    fn rms(samples: &[f32]) -> f64 {
        let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        (sum / f64::from(u32::try_from(samples.len()).unwrap_or(1))).sqrt()
    }

    #[test]
    fn tone_and_mark_decode_deterministically_at_48k_and_16k() {
        for rate in [OutputRate::Hz48000, OutputRate::Hz16000] {
            let mut a = Decoder::new(rate);
            let mut b = Decoder::new(rate);
            for frame in tone().chain(mark()) {
                let sa = a.decode(frame).unwrap().to_vec();
                let sb = b.decode(frame).unwrap().to_vec();
                assert_eq!(sa.len(), rate.frame_samples());
                assert_eq!(sa, sb);
                assert!(sa.iter().all(|s| s.is_finite()));
            }
        }
        assert_eq!(OutputRate::Hz48000.frame_samples(), SAMPLES_48K);
        assert_eq!(OutputRate::Hz16000.frame_samples(), SAMPLES_16K);
    }

    #[test]
    fn rejected_packets_leave_decoder_state_untouched() {
        let frames: Vec<&[u8]> = tone().collect();
        let (before, after) = frames.split_at(frames.len() / 2);
        let mut dec = Decoder::new_48k();
        let mut twin = Decoder::new_48k();
        for frame in before {
            assert_eq!(dec.decode(frame), twin.decode(frame));
        }

        let oversized = [TOC_CELT_WIDEBAND_20MS_MONO; MAX_PACKET_BYTES + 1];
        assert_eq!(dec.decode(&[]), Err(Error::EmptyPacket));
        assert_eq!(
            dec.decode(&oversized),
            Err(Error::OversizedPacket(MAX_PACKET_BYTES + 1))
        );
        assert_eq!(
            dec.decode(&[0x5d, 0x01, 0x02]),
            Err(Error::UnsupportedToc(0x5d))
        );
        assert_eq!(dec.decode(&[0xb0, 0x00]), Err(Error::UnsupportedToc(0xb0)));
        assert_eq!(dec.decode(&[0xb9, 0x00]), Err(Error::UnsupportedToc(0xb9)));

        for frame in after {
            assert_eq!(dec.decode(frame), twin.decode(frame));
        }
    }

    #[test]
    fn reset_restores_initial_state_bit_identically() {
        let frames: Vec<&[u8]> = tone().collect();
        let mut dec = Decoder::new_48k();
        let mut fresh = Decoder::new_48k();
        for frame in &frames {
            let _ = dec.decode(frame).unwrap();
        }
        let _ = dec.conceal();
        dec.reset();
        for frame in &frames {
            assert_eq!(dec.decode(frame).unwrap(), fresh.decode(frame).unwrap());
        }
    }

    #[test]
    fn silence_frame_and_single_byte_dtx_decode_cleanly() {
        let mut cold = Decoder::new_48k();
        let dtx = cold.decode(&[TOC_CELT_WIDEBAND_20MS_MONO]).unwrap();
        assert_eq!(dtx.len(), SAMPLES_48K);
        assert!(dtx.iter().all(|&s| s == 0.0));

        let mut dec = Decoder::new_48k();
        let silence_pkt = pods_samples::opus::silence();
        assert!(dec.decode(silence_pkt).unwrap().iter().all(|&s| s == 0.0));
        for frame in tone().take(10) {
            let _ = dec.decode(frame).unwrap();
        }
        let first = dec.decode(silence_pkt).unwrap().to_vec();
        assert_eq!(first.len(), SAMPLES_48K);
        assert!(first.iter().all(|s| s.is_finite()));
        let mut settled = Vec::new();
        for _ in 0..8 {
            settled = dec.decode(silence_pkt).unwrap().to_vec();
        }
        assert!(
            rms(&settled) < 1e-6,
            "silence did not decay: {}",
            rms(&settled)
        );
    }

    #[test]
    fn plc_is_continuous_at_both_joins_and_decays_over_burst_loss() {
        let frames: Vec<&[u8]> = tone().collect();
        let mut dec = Decoder::new_48k();
        let mut last = Vec::new();
        for frame in frames.iter().take(25) {
            last = dec.decode(frame).unwrap().to_vec();
        }
        let own_step = max_step(&last);
        let concealed = dec.conceal().to_vec();
        assert_eq!(concealed.len(), SAMPLES_48K);
        let resumed = dec.decode(frames[26]).unwrap().to_vec();
        let join_in = (concealed[0] - last[last.len() - 1]).abs();
        let join_out = (resumed[0] - concealed[concealed.len() - 1]).abs();
        assert!(
            join_in <= own_step,
            "join_in {join_in} > own_step {own_step}"
        );
        assert!(
            join_out <= own_step,
            "join_out {join_out} > own_step {own_step}"
        );
        assert!(max_step(&concealed) > own_step * 0.5);

        // Burst loss of 60 frames must stay finite and decay far below initial energy.
        let first_rms = rms(&concealed);
        let mut tail_rms = first_rms;
        for _ in 0..60 {
            let s = dec.conceal();
            assert!(s.iter().all(|v| v.is_finite()));
            tail_rms = rms(s);
        }
        assert!(
            tail_rms < first_rms * 0.05,
            "PLC did not decay: {tail_rms} vs {first_rms}"
        );
    }

    #[test]
    fn valid_short_and_multibitrate_packets_decode_cleanly() {
        let packets: [&[u8]; 9] = [
            // Sol 15-byte lowband-folding regression packet (`pkt15`).
            &[
                0xb8, 0x7f, 0x54, 0xce, 0xa7, 0x4d, 0x11, 0xf1, 0x21, 0x86, 0xed, 0x67, 0x4e, 0x14,
                0x34,
            ],
            // 20-byte VBR 10 kbps packet.
            &[
                0xb8, 0x1d, 0xd3, 0x42, 0x52, 0x8c, 0xcd, 0x14, 0xed, 0x22, 0xa1, 0x42, 0x58, 0x5d,
                0x43, 0x1d, 0x15, 0xe8, 0xd2, 0x28,
            ],
            // 24-byte VBR 12 kbps packet.
            &[
                0xb8, 0x00, 0x61, 0x49, 0x44, 0x8a, 0x16, 0xcf, 0xc5, 0xfb, 0xd2, 0x4a, 0x6a, 0x33,
                0xaa, 0xb4, 0x4a, 0x36, 0x13, 0xd9, 0x6f, 0x68, 0xbe, 0xdb,
            ],
            // 30-byte 12 kbps packet.
            &[
                0xb8, 0x31, 0x5c, 0xe9, 0xde, 0x6a, 0xbc, 0x67, 0xa3, 0xe1, 0x36, 0x03, 0xa0, 0x6e,
                0x9f, 0xdd, 0x1e, 0x8e, 0x01, 0xd5, 0x66, 0x85, 0x58, 0xff, 0x9c, 0xe5, 0xd6, 0xa6,
                0xa0, 0xbe,
            ],
            // 40-byte 16 kbps packet.
            &[
                0xb8, 0xdb, 0x25, 0xc3, 0x94, 0x8d, 0xd8, 0x07, 0x56, 0xda, 0x7a, 0x9f, 0x55, 0x24,
                0x98, 0x74, 0x46, 0xb5, 0xff, 0xeb, 0x77, 0xd2, 0xba, 0x9d, 0x85, 0x29, 0x29, 0xd5,
                0xe2, 0xd6, 0xbd, 0x47, 0xa2, 0x91, 0x06, 0x87, 0x21, 0xe1, 0x03, 0x07,
            ],
            // 60-byte 24 kbps packet exercising skipped-band `fine_priority`.
            &[
                0xb8, 0x31, 0x5c, 0xe9, 0xde, 0xc3, 0xd6, 0xb9, 0xce, 0xc7, 0x3a, 0xdd, 0x84, 0xb1,
                0xc1, 0xb9, 0x5b, 0x60, 0x30, 0x1e, 0x54, 0x2d, 0xd9, 0x15, 0xa1, 0xd9, 0x94, 0x66,
                0xd9, 0xb7, 0x35, 0xe1, 0x72, 0xd6, 0xa1, 0x2a, 0x42, 0x8c, 0xa6, 0x3d, 0x01, 0xe2,
                0xf4, 0x06, 0x72, 0x55, 0x3f, 0x08, 0x13, 0xf3, 0x8c, 0x3e, 0x68, 0x7e, 0x94, 0xe5,
                0x14, 0x2c, 0x62, 0xbc,
            ],
            // 80-byte 32 kbps tonal chirp packet exercising `cap` and `tf = -2`.
            &[
                0xb8, 0xc0, 0x7f, 0x1f, 0x56, 0x99, 0xf9, 0xc6, 0x7c, 0x41, 0x4a, 0xbd, 0x30, 0x60,
                0xab, 0x0d, 0x60, 0x3e, 0xd8, 0xeb, 0x30, 0x3e, 0x97, 0x7c, 0x68, 0xa7, 0xa4, 0x02,
                0x72, 0x87, 0xec, 0xb9, 0x91, 0xd0, 0x06, 0xa0, 0x15, 0x42, 0x46, 0x98, 0x06, 0x51,
                0x69, 0x47, 0x3e, 0x5d, 0x81, 0x0f, 0x88, 0x40, 0x4e, 0x69, 0x76, 0x37, 0x76, 0x41,
                0x7c, 0xe3, 0xe3, 0x6b, 0xa5, 0x3d, 0x0c, 0x6e, 0x61, 0x47, 0x28, 0x0f, 0xe9, 0x78,
                0x43, 0x2e, 0x05, 0x8a, 0x2d, 0xb4, 0x33, 0x00, 0x7e, 0x14,
            ],
            // 90-byte 36 kbps packet exercising `bitexact_log2tan` rounding.
            &[
                0xb8, 0x9c, 0x4a, 0xc7, 0x24, 0x11, 0x87, 0x81, 0x42, 0x70, 0x66, 0x33, 0xf0, 0x0e,
                0x2a, 0x9d, 0x76, 0xf5, 0x67, 0xf2, 0x2a, 0x79, 0x24, 0xd5, 0x48, 0x0a, 0x38, 0xc3,
                0xe2, 0xb0, 0xb7, 0x2f, 0xb3, 0xa2, 0x7b, 0x52, 0xfe, 0xa9, 0x8e, 0x4e, 0x97, 0xb6,
                0xe2, 0xc2, 0x91, 0xfd, 0x81, 0x4c, 0xe9, 0x1b, 0x96, 0xa5, 0xda, 0x87, 0xc7, 0xb2,
                0x8d, 0xaa, 0xcb, 0xd3, 0x0e, 0x65, 0xc8, 0x59, 0xcc, 0x47, 0x53, 0xf6, 0xa5, 0xf0,
                0x9f, 0x7d, 0x84, 0x48, 0xea, 0xc2, 0xca, 0x3d, 0x7a, 0xe9, 0x1e, 0x7c, 0x1b, 0xa9,
                0x51, 0xa3, 0xb1, 0x6c, 0xda, 0x82,
            ],
            // 90-byte 36 kbps packet exercising top-down threshold clamping.
            &[
                0xb8, 0x02, 0x16, 0x5d, 0xc7, 0x6a, 0x54, 0xfe, 0xb0, 0x69, 0xea, 0xde, 0x06, 0x58,
                0x79, 0xc0, 0xbb, 0x59, 0x27, 0x47, 0x46, 0x31, 0xb5, 0x85, 0xcd, 0x79, 0xe9, 0x62,
                0x4f, 0x5a, 0x8e, 0xb9, 0x33, 0x83, 0x10, 0x24, 0x9c, 0x0f, 0x70, 0xee, 0xe8, 0x57,
                0xaa, 0x51, 0x4e, 0x93, 0x22, 0x7d, 0x77, 0xda, 0x85, 0xa6, 0x84, 0xb2, 0xa5, 0x43,
                0x1b, 0xbe, 0x98, 0xf9, 0xd8, 0xfe, 0xd4, 0x29, 0xcb, 0x9c, 0x66, 0x9e, 0xdc, 0x5f,
                0x9d, 0x38, 0x5f, 0x4e, 0xa3, 0x81, 0x3b, 0x6c, 0x36, 0xd8, 0x32, 0x51, 0x91, 0xa0,
                0x9e, 0xad, 0x66, 0xbd, 0x7b, 0x61,
            ],
        ];
        for rate in [OutputRate::Hz48000, OutputRate::Hz16000] {
            let mut a = Decoder::new(rate);
            let mut b = Decoder::new(rate);
            for pkt in packets {
                let sa = a.decode(pkt).unwrap().to_vec();
                let sb = b.decode(pkt).unwrap().to_vec();
                assert_eq!(sa.len(), rate.frame_samples());
                assert_eq!(sa, sb);
                assert!(sa.iter().all(|v| v.is_finite()));
            }
        }
    }

    #[test]
    fn dec_uint_saturation_pkt19_matches_rfc6716_libopus_and_ffopus() {
        let mut state = 0x5eed_c0de_1234_5678_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut pkt19 = [0_u8; 81];
        for idx in 0..20_usize {
            pkt19[0] = TOC_CELT_WIDEBAND_20MS_MONO;
            for b in &mut pkt19[1..] {
                *b = next().to_le_bytes()[0];
            }
            if idx == 19 {
                break;
            }
        }
        let mut dec = Decoder::new_48k();
        let samples = dec.decode(&pkt19).unwrap();
        assert_eq!(samples.len(), SAMPLES_48K);
        assert!(samples.iter().all(|v| v.is_finite()));
        // Black-box `libopus` float and `ffopus` both saturate `dec_uint` (`t = ft - 1`) and output `max_abs = 0.004969`.
        let max_abs = samples.iter().map(|v| v.abs()).fold(0.0_f32, f32::max);
        assert!(
            (f64::from(max_abs) - 0.004_969).abs() < 1e-4,
            "unexpected pkt19 peak {max_abs}"
        );
    }

    #[test]
    fn hostile_smoke_10k_never_panics_and_emits_finite_pcm() {
        let mut dec48 = Decoder::new_48k();
        let mut dec16 = Decoder::new(OutputRate::Hz16000);
        let mut state = 0x5eed_c0de_1234_5678_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut buf = [0_u8; 160];
        for i in 0..10_000_usize {
            if i % 11 == 0 {
                let s = if i % 22 == 0 {
                    dec16.conceal()
                } else {
                    dec48.conceal()
                };
                assert!(s.iter().all(|v| v.is_finite()));
                continue;
            }
            let len = usize::try_from(next() % 161).unwrap_or(0);
            for b in &mut buf[..len] {
                *b = next().to_le_bytes()[0];
            }
            if i % 2 == 0 && len > 0 {
                buf[0] = TOC_CELT_WIDEBAND_20MS_MONO;
            }
            let pkt = &buf[..len];
            let (res, expected_len) = if i % 5 == 0 {
                (dec16.decode(pkt), SAMPLES_16K)
            } else {
                (dec48.decode(pkt), SAMPLES_48K)
            };
            if len == 0 {
                assert_eq!(res, Err(Error::EmptyPacket));
            } else if len > MAX_PACKET_BYTES {
                assert_eq!(res, Err(Error::OversizedPacket(len)));
            } else if buf[0] != TOC_CELT_WIDEBAND_20MS_MONO {
                assert_eq!(res, Err(Error::UnsupportedToc(buf[0])));
            } else {
                let samples = res.unwrap();
                assert_eq!(samples.len(), expected_len);
                assert!(samples.iter().all(|v| v.is_finite()));
            }
        }
    }

    #[test]
    #[ignore = "long-running 1M+ hostile fuzz stress test for pods-opus (--release)"]
    fn fuzz_one_million_hostile_packets() {
        let mut dec48 = Decoder::new_48k();
        let mut dec16 = Decoder::new(OutputRate::Hz16000);
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut buf = [0_u8; 256];
        for case in 0..1_050_000_usize {
            let t0 = Instant::now();
            if case % 10 == 9 {
                let s = if case % 20 == 9 {
                    dec16.conceal()
                } else {
                    dec48.conceal()
                };
                assert!(s.iter().all(|v| v.is_finite()));
                black_box(s[0]);
            } else {
                let len = usize::try_from(next() % 128).unwrap_or(0);
                for b in &mut buf[..len] {
                    *b = next().to_le_bytes()[0];
                }
                if case % 3 != 0 && len > 0 {
                    buf[0] = TOC_CELT_WIDEBAND_20MS_MONO;
                }
                let pkt = &buf[..len];
                let (res, expected_len) = if case % 17 == 0 {
                    (dec16.decode(pkt), SAMPLES_16K)
                } else {
                    (dec48.decode(pkt), SAMPLES_48K)
                };
                if len == 0 {
                    assert_eq!(res, Err(Error::EmptyPacket));
                } else if len > MAX_PACKET_BYTES {
                    assert_eq!(res, Err(Error::OversizedPacket(len)));
                } else if buf[0] != TOC_CELT_WIDEBAND_20MS_MONO {
                    assert_eq!(res, Err(Error::UnsupportedToc(buf[0])));
                } else {
                    let samples = res.unwrap();
                    assert_eq!(samples.len(), expected_len);
                    assert!(samples.iter().all(|v| v.is_finite()));
                    black_box(samples[0]);
                }
            }
            assert!(t0.elapsed().as_millis() <= 100);
            if case % 50_000 == 49_999 {
                dec48.reset();
                dec16.reset();
            }
        }
    }

    proptest! {
        #[test]
        fn arbitrary_packets_never_panic(
            packets in proptest::collection::vec(
                (proptest::collection::vec(any::<u8>(), 0..=120), any::<bool>()),
                1..16
            )
        ) {
            let mut dec = Decoder::new_48k();
            for (mut pkt, force_b8) in packets {
                if force_b8 && let Some(first) = pkt.first_mut() {
                    *first = TOC_CELT_WIDEBAND_20MS_MONO;
                }
                if let Ok(samples) = dec.decode(&pkt) {
                    prop_assert_eq!(samples.len(), SAMPLES_48K);
                    prop_assert!(samples.iter().all(|v| v.is_finite()));
                }
            }
        }
    }
}
