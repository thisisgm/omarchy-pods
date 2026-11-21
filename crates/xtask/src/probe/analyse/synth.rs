//! A seeded generator of uplink capture logs, shared by the unit tests and the test of the built command.

use pods_proto::sdulog::{Direction, Record};
use pods_proto::uplink::{AccessUnit, OPAQUE_LEN, encode_audio};

use super::xorshift::next_random;

/// Filler for header bytes 8 to 21: it holds no true length, so the length rows of a generated stream agree with no SDU.
pub(crate) const OPAQUE: [u8; OPAQUE_LEN] = [
    0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad,
];
/// The shortest coded unit, which decodes to silence.
pub(crate) const SILENT_UNIT: [u8; 2] = [0x00, 0x00];
pub(crate) const NS_PER_SECOND: u64 = 1_000_000_000;
pub(crate) const NS_PER_MS: u64 = 1_000_000;

/// Silent audio SDUs from a peer clocked at `rate_hz`: SDU k arrives as its last unit ends plus a seeded delay under `jitter_ms`, which must not exceed one SDU or the arrivals leave their order.
pub(crate) fn synth(
    rate_hz: u32,
    samples_per_unit: u32,
    units_per_sdu: u32,
    seconds: u32,
    jitter_ms: u32,
    seed: u64,
) -> Vec<Record> {
    let samples_per_sdu = u128::from(samples_per_unit).saturating_mul(u128::from(units_per_sdu));
    let samples = u128::from(seconds).saturating_mul(u128::from(rate_hz));
    let sdus = samples.checked_div(samples_per_sdu).unwrap();
    let delay_limit_ns = u64::from(jitter_ms).saturating_mul(NS_PER_MS);
    let mut random = seed;
    let mut stamp = 0_u32;
    let mut records = Vec::new();
    for k in 1..=sdus {
        let ended_ns = k
            .saturating_mul(samples_per_sdu)
            .saturating_mul(u128::from(NS_PER_SECOND));
        let ideal_ns = ended_ns.checked_div(u128::from(rate_hz)).unwrap();
        let delay_ns = next_random(&mut random)
            .checked_rem(delay_limit_ns)
            .unwrap_or(0);
        let mut units = Vec::new();
        for _ in 0..units_per_sdu {
            units.push(AccessUnit {
                stamp,
                data: &SILENT_UNIT,
            });
            stamp = stamp.wrapping_add(samples_per_unit);
        }
        records.push(Record {
            at_ns: u64::try_from(ideal_ns).unwrap().saturating_add(delay_ns),
            direction: Direction::FromPods,
            sdu: encode_audio(&OPAQUE, &units).unwrap(),
        });
    }
    records
}

/// Whether `text` holds `private` as it is or as the lowercase hex a packet row prints.
pub(crate) fn shows(text: &str, private: &str) -> bool {
    let hex: Vec<String> = private.bytes().map(|byte| format!("{byte:02x}")).collect();
    text.contains(private) || text.contains(&hex.concat())
}
