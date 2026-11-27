//! K8 proof: zero heap allocations across 1,000 decodes after warm-up, measured under a counting global allocator.

#[cfg(test)]
mod tests {
    use std::hint::black_box;

    use pods_aac::encode::{BitWriter, emit_cb1_quad};
    use pods_aac::{Decoder, EXPECTED_CONFIG, SAMPLES_PER_UNIT};

    const SILENT_UNIT: [u8; 2] = [0x00, 0x00];
    const TONE_UNIT: [u8; 4] = [0xb4, 0x04, 0x42, 0x50];
    const DECODES: usize = 1000;

    fn tns_unit() -> Vec<u8> {
        let mut bw = BitWriter::new();
        bw.write(160, 8);
        bw.write(2, 6);
        bw.write(1, 4);
        bw.write(2, 5);
        bw.write_bool(false);
        bw.write_bool(false);
        bw.write_bool(true);
        bw.write(1, 2);
        bw.write_bool(true);
        bw.write(35, 6);
        bw.write(3, 5);
        bw.write_bool(false);
        bw.write_bool(false);
        bw.write(2, 4);
        bw.write(14, 4);
        bw.write(1, 4);
        emit_cb1_quad([1, 0, -1, 0], &mut bw);
        emit_cb1_quad([0, 1, 0, -1], &mut bw);
        bw.into_bytes()
    }

    #[test]
    fn decode_never_allocates_across_1000_units_after_warmup() {
        let mut decoder = Decoder::with_config(&EXPECTED_CONFIG).unwrap();
        let tns = tns_unit();

        // Warm-up decodes ensure any one-time initialization is outside the measured span.
        assert_eq!(decoder.decode(&TONE_UNIT).unwrap().len(), SAMPLES_PER_UNIT);
        assert_eq!(decoder.decode(&tns).unwrap().len(), SAMPLES_PER_UNIT);
        assert_eq!(
            decoder.decode(&SILENT_UNIT).unwrap().len(),
            SAMPLES_PER_UNIT
        );

        let inputs: [&[u8]; 4] = [&TONE_UNIT, &tns, &SILENT_UNIT, &[0xff, 0xff]];
        let mut ok_count = 0_usize;
        let measured = allocation_counter::measure(|| {
            for i in 0..DECODES {
                let unit = inputs[i % inputs.len()];
                if black_box(decoder.decode(unit)).is_ok() {
                    ok_count += 1;
                }
            }
        });
        assert_eq!(ok_count, DECODES * 3 / 4);
        assert_eq!(
            measured.count_total, 0,
            "decode allocated {} times across {DECODES} units",
            measured.count_total
        );

        // Positive control: prove the counting allocator is active in this binary.
        let control = allocation_counter::measure(|| drop(black_box(vec![0_u8; 64])));
        assert_eq!(control.count_total, 1);
    }
}
