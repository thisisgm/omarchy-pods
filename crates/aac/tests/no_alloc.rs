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
        let mut writer = BitWriter::new();
        writer.write(160, 8); // global_gain
        writer.write(2, 6); // max_sfb
        writer.write(1, 4); // codebook 1
        writer.write(2, 5); // section of 2 bands
        writer.write_bool(false); // scalefactor delta 0
        writer.write_bool(false); // scalefactor delta 0
        writer.write_bool(true); // tns_present
        writer.write(1, 2); // n_filt
        writer.write_bool(true); // 4-bit coefficients
        writer.write(35, 6); // length
        writer.write(3, 5); // order
        writer.write_bool(false); // upward
        writer.write_bool(false); // no compression
        writer.write(2, 4); // coefficient +2
        writer.write(14, 4); // coefficient -2
        writer.write(1, 4); // coefficient +1
        emit_cb1_quad([1, 0, -1, 0], &mut writer);
        emit_cb1_quad([0, 1, 0, -1], &mut writer);
        writer.into_bytes()
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
