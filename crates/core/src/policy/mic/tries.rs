//! The tries of a START: how many go out before the pods' silence is their answer, the lines each one logs, and the stall of a running uplink that sends one more.

use std::time::Duration;

use super::SLOWEST_START_ANSWER_MS;

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

/// About fourteen times the largest gap measured between two packets.
pub const UPLINK_STALL_MS: u64 = 1000;

// The next read answers a stall's START, so a stall window outlasts the slowest START answer.
const _: () = assert!(UPLINK_STALL_MS > SLOWEST_START_ANSWER_MS);

#[must_use]
pub fn stalled_line(for_ms: u128) -> String {
    format!("mic: stalled, no audio for {for_ms} ms")
}

/// The stall check of a running uplink: the audio packets counted at its last read, and when it reads again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Watch {
    /// None before the first read of the run, and after a time with no bud worn.
    packets: Option<u64>,
    /// When the count last moved, or the check began.
    moved: Duration,
    /// None while no bud is worn, when the pods send no audio to count.
    due: Option<Duration>,
    /// The START that went out last: a stall sends the next while one is left.
    tried: Try,
    /// A stall's START still out, until its answer window ends.
    retried: Option<Retry>,
}

/// A stall's START out since `sent`; `owed` once a STOP waits for its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Retry {
    sent: Duration,
    owed: bool,
}

/// What one read of the count found.
pub(super) enum Read {
    Moved(Watch),
    /// The count did not move for `for_ms`; `next` is the try to send, none once they are spent.
    Stalled {
        watch: Watch,
        for_ms: u128,
        next: Option<Try>,
    },
}

impl Watch {
    /// The check of a run the START of `tried` began, reading its first count at once.
    pub(super) fn new(tried: Try, now: Duration) -> Self {
        Self {
            packets: None,
            moved: now,
            due: Some(now),
            tried,
            retried: None,
        }
    }

    pub(super) fn deadline(self) -> Option<Duration> {
        self.due.into_iter().chain(self.answer_by()).min()
    }

    /// The watch once a stall's START of `tried` went out at `now`.
    pub(super) fn retry(self, tried: Try, now: Duration) -> Self {
        Self {
            tried,
            retried: Some(Retry {
                sent: now,
                owed: false,
            }),
            ..self
        }
    }

    /// A stall's START is out and unanswered, so a STOP would overtake it.
    pub(super) fn retrying(self) -> bool {
        self.retried.is_some()
    }

    /// Whether a STOP already waits for the stall's START.
    pub(super) fn owes(self) -> bool {
        self.retried.is_some_and(|retry| retry.owed)
    }

    /// The watch once a STOP waits for the stall's START.
    pub(super) fn owe(self) -> Self {
        Self {
            retried: self.retried.map(|retry| Retry {
                owed: true,
                ..retry
            }),
            ..self
        }
    }

    /// The deadline of an unanswered stall START, one stall window after it went out.
    pub(super) fn answer_by(self) -> Option<Duration> {
        let window = Duration::from_millis(UPLINK_STALL_MS);
        self.retried.map(|retry| retry.sent.saturating_add(window))
    }

    /// Off while no bud is worn; a bud worn again starts the count anew at once; a stall's START still out when its answer window ends is answered by silence.
    pub(super) fn follow(self, worn: bool, now: Duration) -> Self {
        let this = if self.answer_by().is_some_and(|by| now >= by) {
            Self {
                retried: None,
                ..self
            }
        } else {
            self
        };
        match (worn, this.due) {
            (false, _) => Self {
                packets: None,
                due: None,
                ..this
            },
            (true, None) => Self {
                moved: now,
                due: Some(now),
                ..this
            },
            (true, Some(_)) => this,
        }
    }

    /// The count read at `now`; none before the check is due.
    pub(super) fn read(self, packets: u64, now: Duration) -> Option<Read> {
        if self.due.is_none_or(|due| now < due) {
            return None;
        }
        let due = now.checked_add(Duration::from_millis(UPLINK_STALL_MS));
        let read = Self {
            packets: Some(packets),
            due,
            ..self
        };
        Some(match self.packets {
            Some(last) if last == packets => Read::Stalled {
                watch: read,
                for_ms: now.saturating_sub(self.moved).as_millis(),
                next: self.tried.next(),
            },
            Some(_) => Read::Moved(Self {
                moved: now,
                tried: Try::FIRST,
                ..read
            }),
            None => Read::Moved(Self { moved: now, ..read }),
        })
    }
}
