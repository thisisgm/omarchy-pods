//! Builders the unit tests of the analysis share.

use pods_proto::sdulog::{Direction, Record};
use pods_proto::uplink::{AccessUnit, OPAQUE_LEN, encode_audio};

use super::input::{Mark, MarkKind};
pub(crate) use super::synth::{NS_PER_MS, NS_PER_SECOND, OPAQUE, SILENT_UNIT, shows};

/// The seed every generated log in the unit tests uses.
pub(crate) const SEED: u64 = 0x5eed_0001;
/// The stamp step of the generated units, one 480 sample frame.
pub(crate) const STAMP_STEP: u32 = 480;
/// The arrival interval of the hand built streams, four 7.5 ms units.
pub(crate) const SDU_INTERVAL_NS: u64 = 30 * NS_PER_MS;
/// Units in each SDU of the hand built streams.
pub(crate) const UNITS_PER_SDU: u32 = 4;

pub(crate) fn from_pods(at_ns: u64, sdu: &[u8]) -> Record {
    Record {
        at_ns,
        direction: Direction::FromPods,
        sdu: sdu.to_vec(),
    }
}

pub(crate) fn to_pods(at_ns: u64, sdu: &[u8]) -> Record {
    Record {
        at_ns,
        direction: Direction::ToPods,
        sdu: sdu.to_vec(),
    }
}

/// One audio SDU from the pods holding `units`, each a stamp and its coded bytes.
pub(crate) fn audio(at_ns: u64, opaque: &[u8; OPAQUE_LEN], units: &[(u32, &[u8])]) -> Record {
    let units: Vec<AccessUnit<'_>> = units
        .iter()
        .map(|&(stamp, data)| AccessUnit { stamp, data })
        .collect();
    from_pods(at_ns, &encode_audio(opaque, &units).unwrap())
}

/// A steady stream of `sdus` SDUs of four silent units, SDU k, counted from 0, at (k + 1) x 30 ms, with the opaque bytes `opaque_of(k)`.
pub(crate) fn stream(sdus: u32, opaque_of: impl Fn(u32) -> [u8; OPAQUE_LEN]) -> Vec<Record> {
    let mut stamp = 0_u32;
    (0..sdus)
        .map(|k| {
            let units: Vec<(u32, &[u8])> = (0..UNITS_PER_SDU)
                .map(|_| {
                    let unit = (stamp, &SILENT_UNIT[..]);
                    stamp = stamp.wrapping_add(STAMP_STEP);
                    unit
                })
                .collect();
            let at_ns = u64::from(k)
                .saturating_add(1)
                .saturating_mul(SDU_INTERVAL_NS);
            audio(at_ns, &opaque_of(k), &units)
        })
        .collect()
}

pub(crate) fn mark(at_ns: u64, kind: MarkKind) -> Mark {
    Mark { at_ns, kind }
}

/// Moves every record `by_ns` later.
pub(crate) fn shifted(mut records: Vec<Record>, by_ns: u64) -> Vec<Record> {
    for record in &mut records {
        record.at_ns = record.at_ns.saturating_add(by_ns);
    }
    records
}

/// Holds back every record that arrived in the `for_ns` after `from_ns` and delivers them together when it ends.
pub(crate) fn stall(records: &mut [Record], from_ns: u64, for_ns: u64) {
    let until_ns = from_ns.saturating_add(for_ns);
    for record in records {
        if (from_ns..until_ns).contains(&record.at_ns) {
            record.at_ns = until_ns;
        }
    }
}

pub(crate) fn close(a: f64, b: f64, tolerance: f64) -> bool {
    (a - b).abs() <= tolerance
}

/// How far `measured` is from `truth`, in parts per million of `truth`.
pub(crate) fn ppm_apart(measured: f64, truth: f64) -> f64 {
    ((measured - truth) / truth * 1e6).abs()
}
