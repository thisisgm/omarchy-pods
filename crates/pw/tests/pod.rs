//! Support tests: the two messages the node sends, read back by the rig's reader, which shares no line with the writer; no user path yet.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use pods_pw::pod::{format, process_latency};
    use pods_pwrig::pod::{read_format, read_process_latency};
    use proptest::prelude::*;

    const RATE: u32 = 64_000;
    const LATENCY_NS: u64 = 85_000_000;
    /// The length of the one format message a reader accepts, and of the one latency message.
    const FORMAT_BYTES: usize = 168;
    const LATENCY_BYTES: usize = 88;
    /// Every bit of a byte.
    const BITS: [u8; 8] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80];
    /// A property with an empty value: key, flags, size and type, all zero.
    const EMPTY_PROPERTY: [u8; 16] = [0; 16];

    proptest! {
        #[test]
        fn a_format_reads_back_at_its_rate(rate in any::<u32>()) {
            prop_assert_eq!(format(rate).len(), FORMAT_BYTES);
            prop_assert_eq!(read_format(&format(rate)), Some(rate));
            prop_assert_eq!(read_process_latency(&format(rate)), None);
        }

        #[test]
        fn a_process_latency_reads_back_at_its_nanoseconds(ns in any::<u64>()) {
            prop_assert_eq!(process_latency(ns).len(), LATENCY_BYTES);
            prop_assert_eq!(read_process_latency(&process_latency(ns)), Some(ns));
            prop_assert_eq!(read_format(&process_latency(ns)), None);
        }

        #[test]
        fn neither_reader_panics_or_takes_another_length(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            prop_assert!(read_format(&bytes).is_none() || bytes.len() == FORMAT_BYTES);
            prop_assert!(read_process_latency(&bytes).is_none() || bytes.len() == LATENCY_BYTES);
        }
    }

    #[test]
    fn a_message_with_any_one_bit_changed_does_not_read_as_itself() {
        for at in 0..FORMAT_BYTES {
            for bit in BITS {
                let mut message = format(RATE);
                message[at] ^= bit;
                assert_ne!(read_format(&message), Some(RATE), "byte {at}");
            }
        }
        for at in 0..LATENCY_BYTES {
            for bit in BITS {
                let mut message = process_latency(LATENCY_NS);
                message[at] ^= bit;
                assert_ne!(
                    read_process_latency(&message),
                    Some(LATENCY_NS),
                    "byte {at}"
                );
            }
        }
    }

    #[test]
    fn a_message_cut_short_reads_as_nothing() {
        for cut in 0..FORMAT_BYTES {
            assert_eq!(read_format(&format(RATE)[..cut]), None, "{cut} bytes");
        }
        for cut in 0..LATENCY_BYTES {
            let message = process_latency(LATENCY_NS);
            assert_eq!(read_process_latency(&message[..cut]), None, "{cut} bytes");
        }
    }

    #[test]
    fn a_message_with_one_more_property_reads_as_nothing() {
        assert_eq!(read_format(&longer(format(RATE))), None);
        assert_eq!(
            read_process_latency(&longer(process_latency(LATENCY_NS))),
            None
        );
    }

    /// `message` with an empty property after its last, and its size word raised to match.
    fn longer(mut message: Vec<u8>) -> Vec<u8> {
        let size = u8::try_from(EMPTY_PROPERTY.len()).unwrap();
        message[0] = message[0].checked_add(size).unwrap();
        message.extend_from_slice(&EMPTY_PROPERTY);
        message
    }
}
