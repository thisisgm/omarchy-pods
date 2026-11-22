//! The arrivals of a generated stream that opens with a backlog, shared by the unit tests of the coarse clock and the test of the built commands.

use pods_proto::sdulog::Record;

/// `steady` as a peer delivers it that recorded from `lead_ns` before it could send: SDU k, counted from zero, arrives once its last unit has ended and no sooner than k x `pace_ns`, and none at `stop_ns` or later.
pub(crate) fn with_backlog(
    steady: Vec<Record>,
    lead_ns: u64,
    pace_ns: u64,
    stop_ns: u64,
) -> Vec<Record> {
    let arrivals = (0_u64..).zip(steady).map(|(turn, mut record)| {
        let ended_ns = record.at_ns.saturating_sub(lead_ns);
        record.at_ns = ended_ns.max(turn.saturating_mul(pace_ns));
        record
    });
    arrivals
        .take_while(|record| record.at_ns < stop_ns)
        .collect()
}
