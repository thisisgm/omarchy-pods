//! The seeded generator behind every generated log and every mutated unit.

/// One step of a generator made from xorshift64, which never leaves zero: the low bit is set before every step, so no state is zero.
pub(crate) fn next_random(state: &mut u64) -> u64 {
    let mut x = *state | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}
