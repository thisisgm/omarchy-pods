//! The tries of a START: how many go out before the pods' silence is their answer, and the lines each one logs.

/// A radio hiccup is one try; three silences are an answer.
pub const START_TRIES: u32 = 3;

#[must_use]
pub fn start_sent_line(tried: u32) -> String {
    format!("mic: start sent (try {tried})")
}

#[must_use]
pub fn unanswered_line(tried: u32) -> String {
    format!("mic: start unanswered (try {tried} of {START_TRIES})")
}

/// Which START of at most `START_TRIES` is out; only `FIRST` and `next` make one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Try(u32);

impl Try {
    pub(super) const FIRST: Self = Self(1);

    /// The try after this one, while one is left.
    pub(super) fn next(self) -> Option<Self> {
        (self.0 < START_TRIES).then_some(Self(self.0.saturating_add(1)))
    }

    pub(super) fn number(self) -> u32 {
        self.0
    }
}
