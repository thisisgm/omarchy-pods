//! When an uplink nobody wants any more is stopped: the linger, the grace of a bud taken out, and the gate a worn bud's playback holds shut.

use std::time::Duration;

use super::Word;

/// So two dictations a minute apart share one start.
pub const UPLINK_LINGER_MS: u64 = 60_000;
/// A bud adjusted is not a bud removed.
pub const BUD_GRACE_MS: u64 = 4000;

pub const STOP_WAITS_LINE: &str = "mic: stop waits for playback";

/// Why a running uplink is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Hold {
    Wanted,
    /// Unwanted from `since`, and no bud worn from `unworn`.
    Lingering {
        since: Duration,
        unworn: Option<Duration>,
    },
    /// The STOP is due and the gate is shut; no bud worn from `unworn`.
    Waiting {
        unworn: Option<Duration>,
    },
}

/// What a running uplink does after a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Next {
    Hold(Hold),
    Stop(Word),
}

impl Hold {
    /// `quiet` is a followed sink known to be suspended or absent.
    pub(super) fn follow(self, wanted: bool, worn: bool, quiet: bool, now: Duration) -> Next {
        if wanted {
            return Next::Hold(Self::Wanted);
        }
        let hold = match self {
            Self::Wanted => Self::Lingering {
                since: now,
                unworn: (!worn).then_some(now),
            },
            Self::Lingering { since, unworn } => Self::Lingering {
                since,
                unworn: left(worn, unworn, now),
            },
            Self::Waiting { unworn } => Self::Waiting {
                unworn: left(worn, unworn, now),
            },
        };
        if hold.deadline().is_some_and(|deadline| now < deadline) {
            Next::Hold(hold)
        } else if !worn {
            Next::Stop(Word::NotWorn)
        } else if quiet {
            Next::Stop(Word::Quiet)
        } else {
            Next::Hold(Self::Waiting { unworn: None })
        }
    }

    pub(super) fn deadline(self) -> Option<Duration> {
        let grace = |left: Duration| left.saturating_add(Duration::from_millis(BUD_GRACE_MS));
        match self {
            Self::Wanted => None,
            Self::Lingering { since, unworn } => {
                let linger = since.saturating_add(Duration::from_millis(UPLINK_LINGER_MS));
                Some(unworn.map_or(linger, |left| grace(left).min(linger)))
            }
            Self::Waiting { unworn } => unworn.map(grace),
        }
    }
}

/// When the last bud left, kept while none is worn.
fn left(worn: bool, unworn: Option<Duration>, now: Duration) -> Option<Duration> {
    if worn { None } else { unworn.or(Some(now)) }
}
