//! Swaps the units of a generated stream, so a run can hold audio that is not silence.

use pods_proto::sdulog::Record;
use pods_proto::uplink::{AccessUnit, UplinkPacket, encode_audio};

/// The same stream with what `unit_of` gives in place of each unit in turn, stamps and arrival times kept.
pub(crate) fn with_units<'unit>(
    records: &[Record],
    mut unit_of: impl FnMut() -> &'unit [u8],
) -> Vec<Record> {
    let mut replaced = |record: &Record| {
        let Ok(Some(UplinkPacket::Audio(audio))) = UplinkPacket::parse(&record.sdu) else {
            panic!("the generator makes audio SDUs only");
        };
        let units: Vec<AccessUnit<'_>> = audio
            .units()
            .map(|coded| AccessUnit {
                stamp: coded.stamp,
                data: unit_of(),
            })
            .collect();
        Record {
            at_ns: record.at_ns,
            direction: record.direction,
            sdu: encode_audio(audio.opaque, &units).unwrap(),
        }
    };
    records.iter().map(&mut replaced).collect()
}
