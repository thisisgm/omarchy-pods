//! The suspend window: from logind's edge into sleep until a settle after the wake, a link that goes down stays silent.

use std::time::Duration;

/// The C++ `onSystemWakingUp` waits this long before it touches Bluetooth, for BlueZ and the controller to answer again.
pub(super) const RESUME_SETTLE_MS: u64 = 2000;

/// logind's `PrepareForSleep`: `true` is `Sleeping`, `false` is `Woke`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepEdge {
    Sleeping,
    Woke,
}

/// Only the latest window is kept, a closed one only while the link stays down after it, so an edge then extends it for the loss that may await its reset.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum Suspend {
    #[default]
    Never,
    Asleep {
        opened: Duration,
    },
    Settling {
        opened: Duration,
        closes: Duration,
    },
    Closed {
        opened: Duration,
        closed: Duration,
    },
}

impl Suspend {
    /// An edge keeps the start of the window it finds; a `Woke` with no window seen opens one at the wake.
    pub(super) fn edge(&mut self, edge: SleepEdge, now: Duration) {
        let opened = match *self {
            Self::Asleep { opened }
            | Self::Settling { opened, .. }
            | Self::Closed { opened, .. } => opened,
            Self::Never => now,
        };
        *self = match edge {
            SleepEdge::Sleeping => Self::Asleep { opened },
            SleepEdge::Woke => Self::Settling {
                opened,
                closes: now.saturating_add(Duration::from_millis(RESUME_SETTLE_MS)),
            },
        };
    }

    /// Closes the window once its settle has passed, exactly then answering true.
    pub(super) fn close(&mut self, now: Duration) -> bool {
        let Self::Settling { opened, closes } = *self else {
            return false;
        };
        if now < closes {
            return false;
        }
        *self = Self::Closed {
            opened,
            closed: closes,
        };
        true
    }

    /// The link is up, so no loss from a closed window can still await its reset, and the window is forgotten.
    pub(super) fn relinked(&mut self) {
        if let Self::Closed { .. } = *self {
            *self = Self::Never;
        }
    }

    pub(super) fn deadline(self) -> Option<Duration> {
        match self {
            Self::Settling { closes, .. } => Some(closes),
            Self::Never | Self::Asleep { .. } | Self::Closed { .. } => None,
        }
    }

    /// Whether `at` lies inside the open window or the last one.
    pub(super) fn covers(self, at: Duration) -> bool {
        match self {
            Self::Never => false,
            Self::Asleep { opened } => opened <= at,
            Self::Settling {
                opened,
                closes: closed,
            }
            | Self::Closed { opened, closed } => (opened..=closed).contains(&at),
        }
    }
}

#[cfg(test)]
mod tests;
