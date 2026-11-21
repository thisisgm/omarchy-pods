//! The intervals and bounds of the link's recovery ladders, each with the reason for its value.

/// The C++ daemon asks again after 2 s; the acknowledgement was measured at about 10 ms.
pub const HANDSHAKE_STEP_TIMEOUT_MS: u64 = 2000;

/// The C++ daemon repeats the notification request once, this long after the first.
pub const NOTIFY_REREQUEST_MS: u64 = 2000;

/// The C++ `firstDelayMs`: short enough that a profile rebuild is still under way.
pub const FIRST_RETRY_MS: u64 = 750;

/// The C++ `delayMs(1..=10)` with `connectedAttemptLimit` 10, jitter removed: the wait after failed attempt `n` is entry `n - 1`.
pub const HELD_DELAYS_MS: [u64; 10] = [
    1000, 2000, 4000, 8000, 16000, 16000, 16000, 16000, 16000, 16000,
];

/// The attempt a parked device makes, one past the last held attempt, so a failure parks it again.
pub const PARKED_ATTEMPT: u32 = 12;

/// Twelve short attempts an hour on a device that never offers the channel, and pods released elsewhere return within five minutes.
pub const PARKED_RETRY_MS: u64 = 300_000;

/// The C++ absent ladder as one deadline: 750 + 1000 + 2000 + 4000.
pub const ABSENT_GRACE_MS: u64 = 7750;

/// Commands waiting for a report at once; the next one is refused.
pub const PENDING_MAX: usize = 16;

/// Paired AirPods the machine remembers; once full, a new address is not kept.
pub const KNOWN_MAX: usize = 16;
