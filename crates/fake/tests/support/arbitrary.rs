//! What the fake's arbitrary bytes tests share: the SDUs a host may send, of any bytes.

use proptest::prelude::*;

/// The largest random SDU and the most random SDUs a property test sends.
const SDU_MAX: usize = 4096;
const SDUS_MAX: usize = 50;

// An empty SDU reads as a close on a socket pair, so every SDU has a byte.
pub(crate) fn sdus() -> impl Strategy<Value = Vec<Vec<u8>>> {
    let sdu = prop::collection::vec(any::<u8>(), 1..=SDU_MAX);
    prop::collection::vec(sdu, 0..=SDUS_MAX)
}
