//! The synchronous clock used by the hardware waits.

use std::time::{Duration, Instant};

use super::unit::pause;

pub(super) trait Clock {
    fn now(&self) -> Instant;
    fn pause(&self, duration: Duration);

    fn deadline(&self, timeout: Duration) -> Option<Instant> {
        self.now().checked_add(timeout)
    }

    fn remaining(&self, deadline: Option<Instant>) -> Duration {
        deadline.map_or(Duration::ZERO, |end| {
            end.saturating_duration_since(self.now())
        })
    }
}

pub(super) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn pause(&self, duration: Duration) {
        pause(duration);
    }
}

#[cfg(test)]
pub(super) struct PausedClock {
    start: Instant,
    elapsed: std::cell::Cell<Duration>,
}

#[cfg(test)]
impl Default for PausedClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            elapsed: std::cell::Cell::new(Duration::ZERO),
        }
    }
}

#[cfg(test)]
impl PausedClock {
    pub(super) fn elapsed(&self) -> Duration {
        self.elapsed.get()
    }
}

#[cfg(test)]
impl Clock for PausedClock {
    fn now(&self) -> Instant {
        self.start.checked_add(self.elapsed()).unwrap_or(self.start)
    }

    fn pause(&self, duration: Duration) {
        self.elapsed.set(self.elapsed().saturating_add(duration));
    }
}
