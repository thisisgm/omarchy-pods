//! When an uplink nobody wants any more is stopped: the linger, the grace of a bud taken out, and the gate a worn bud's playback holds shut.

use std::time::Duration;

use super::Word;

/// So two dictations a minute apart share one start.
pub const UPLINK_LINGER_MS: u64 = 60_000;
/// A bud adjusted is not a bud removed.
pub const BUD_GRACE_MS: u64 = 4000;

pub const STOP_WAITS_LINE: &str = "mic: stop waits for playback";
pub const STOP_OWED_PLAYING_LINE: &str = "mic: stop owed, the sink is playing";

/// Why a running uplink is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Hold {
    Wanted,
    /// The pods said they run with no START out, so the want and its linger are over already.
    Late,
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
            Self::Late => return gate(worn, quiet),
            Self::Wanted => Self::Lingering {
                since: now,
                unworn: (!worn).then_some(now),
            },
            Self::Lingering { since, unworn } => Self::Lingering {
                since,
                unworn: left(worn, unworn, now),
            },
            Self::Waiting { .. } if quiet => return Next::Stop(Word::Quiet),
            Self::Waiting { unworn } => Self::Waiting {
                unworn: left(worn, unworn, now),
            },
        };
        if hold.deadline().is_some_and(|deadline| now < deadline) {
            Next::Hold(hold)
        } else {
            gate(worn, quiet)
        }
    }

    /// A STOP owed with no linger, as `off` and the exit owe it; a waiting STOP's bud grace applies while the sink runs.
    pub(super) fn owed(self, worn: bool, quiet: bool, now: Duration) -> Next {
        match self {
            Self::Waiting { .. } => self.follow(false, worn, quiet, now),
            Self::Wanted | Self::Late | Self::Lingering { .. } => gate(worn, quiet),
        }
    }

    pub(super) fn deadline(self) -> Option<Duration> {
        let grace = |left: Duration| left.saturating_add(Duration::from_millis(BUD_GRACE_MS));
        match self {
            Self::Wanted | Self::Late => None,
            Self::Lingering { since, unworn } => {
                let linger = since.saturating_add(Duration::from_millis(UPLINK_LINGER_MS));
                Some(unworn.map_or(linger, |left| grace(left).min(linger)))
            }
            Self::Waiting { unworn } => unworn.map(grace),
        }
    }
}

/// A due STOP goes out with no bud worn or with the sink quiet, and waits otherwise.
fn gate(worn: bool, quiet: bool) -> Next {
    open(worn, quiet).map_or(Next::Hold(Hold::Waiting { unworn: None }), Next::Stop)
}

/// The word a due STOP goes out under; none while a worn bud's sink is not quiet.
pub(super) fn open(worn: bool, quiet: bool) -> Option<Word> {
    if !worn {
        Some(Word::NotWorn)
    } else if quiet {
        Some(Word::Quiet)
    } else {
        None
    }
}

/// When the last bud left, kept while none is worn.
fn left(worn: bool, unworn: Option<Duration>, now: Duration) -> Option<Duration> {
    if worn { None } else { unworn.or(Some(now)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_sink_releases_a_waiting_stop_during_the_bud_grace() {
        let left = Duration::from_secs(1);
        let now = left.saturating_add(Duration::from_millis(BUD_GRACE_MS / 2));
        let hold = Hold::Waiting { unworn: Some(left) };
        assert_eq!(hold.owed(false, false, now), Next::Hold(hold));
        assert_eq!(hold.owed(false, true, now), Next::Stop(Word::Quiet));
        assert_eq!(
            hold.follow(false, false, true, now),
            Next::Stop(Word::Quiet)
        );
    }
}
