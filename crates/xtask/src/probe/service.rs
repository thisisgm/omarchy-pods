//! The installed service around a probe run that dials the pods: stopped under the hardware run's lock, and started again on every way out.

use std::os::fd::OwnedFd;

use crate::hw::run_lock;
use crate::hw::unit::{self, Guard, INSTALLED};

/// Why the installed service was not stopped, or is not as the run found it.
#[derive(Debug, thiserror::Error)]
pub(super) enum ServiceError {
    #[error("a hardware run or another live probe is in progress, so nothing was stopped")]
    LockHeld,
    /// The lock or the state of the service could not be read.
    #[error("{0}; nothing was stopped")]
    NotStopped(String),
    #[error("{0}; {INSTALLED} was started again")]
    StopFailed(String),
    #[error(
        "{0}, so {INSTALLED} is not as the run found it; run systemctl --user start {INSTALLED}"
    )]
    NotRestored(String),
}

/// The hardware run's lock, or `None` for a unit test that touches no service.
pub(super) struct RunLock(Option<OwnedFd>);

impl RunLock {
    /// Refuses a probe while another run holds the lock.
    pub(super) fn take() -> Result<Self, ServiceError> {
        let lock = run_lock()
            .map_err(ServiceError::NotStopped)?
            .ok_or(ServiceError::LockHeld)?;
        Ok(Self(Some(lock)))
    }

    #[cfg(test)]
    pub(super) fn nothing() -> Self {
        Self(None)
    }

    /// Stops the installed service if it runs, keeping the lock through its restore.
    pub(super) fn stop(self) -> Result<Stopped, ServiceError> {
        #[cfg(test)]
        if self.0.is_none() {
            return Ok(Stopped::nothing());
        }
        let word = unit::activity(INSTALLED).map_err(ServiceError::NotStopped)?;
        let was_running = unit::is_running(&word);
        // Made before the stop, as the hardware run makes it, so a stop that fails partway and every later way out, an unwind included, start it again.
        let guard = Guard::installed_only(was_running);
        if was_running && let Err(stop) = unit::stop(INSTALLED) {
            return Err(match guard.restore() {
                Ok(()) => ServiceError::StopFailed(stop),
                Err(start) => ServiceError::NotRestored(format!("{stop}; {start}")),
            });
        }
        Ok(Stopped {
            was_running,
            guard,
            lock: self.0,
        })
    }
}

/// The installed service as a run that dials the pods holds it: stopped if it ran, under the hardware run's lock. Dropping it on any way out, an unwind included, starts the service again.
#[derive(Debug)]
pub(super) struct Stopped {
    was_running: bool,
    // Declared before the lock, so a drop restores while the lock is still held.
    guard: Guard,
    lock: Option<OwnedFd>,
}

impl Stopped {
    /// What a unit test gives its host: nothing stopped, nothing held and nothing to put back.
    #[cfg(test)]
    fn nothing() -> Self {
        Self {
            was_running: false,
            guard: Guard::installed_only(false),
            lock: None,
        }
    }

    /// True when the run stopped the service, so its restore starts it again.
    pub(super) fn was_running(&self) -> bool {
        self.was_running
    }

    /// Starts the service again if it ran, and gives the result to the caller instead of printing it on drop.
    pub(super) fn restore(self) -> Result<(), ServiceError> {
        let restored = self.guard.restore().map_err(ServiceError::NotRestored);
        drop(self.lock);
        restored
    }
}
