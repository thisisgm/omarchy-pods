//! A test tool: runs a child under a seccomp filter and answers its Bluetooth `socket` and `connect` calls with one end of an `AF_UNIX` socket pair.
#![cfg_attr(
    test,
    expect(
        dead_code_pub_in_binary,
        reason = "the unit tests cover the pure parts; the integration tests drive `spawn` and `Trap` through the stand-in"
    )
)]

mod filter;
mod install;
mod notify;
mod record;
mod supervisor;
mod verdict;

use std::convert::Infallible;
use std::fmt;
use std::io;
use std::os::fd::OwnedFd;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pods_proto::addr::BdAddr;
use rustix::event::{EventfdFlags, eventfd};

pub use install::spawn;
pub use record::{Answer, Dial, Outcome, Route, Serve, TrapError, Violation};
use supervisor::Shared;

/// Long enough for a loaded host to fork, install the filter and send one descriptor.
pub const HAND_OVER_WAIT: Duration = Duration::from_secs(5);
/// The thread blocks only in `poll`, so a stop reaches it at once; this bounds a `serve` that does not return.
pub const STOP_WAIT: Duration = Duration::from_secs(2);

/// The handle on the trap's thread; dropping it stops the thread, and the child's later trapped calls then fail with `ENOSYS`.
pub struct Trap {
    shared: Arc<Shared>,
    stop: Arc<OwnedFd>,
    /// The thread, and the channel whose sender it holds, which disconnects when the thread ends.
    thread: Option<(JoinHandle<()>, mpsc::Receiver<Infallible>)>,
    spawned: Instant,
}

impl Trap {
    fn start(listener: OwnedFd, routes: Vec<Route>, spawned: Instant) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        shared.lock().routes = routes;
        let stop = Arc::new(eventfd(0, EventfdFlags::CLOEXEC)?);
        let (alive, ended) = mpsc::channel::<Infallible>();
        let thread = thread::Builder::new().name("trap".to_owned()).spawn({
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            move || {
                let _alive = alive;
                supervisor::run(listener, stop, &shared);
            }
        })?;
        Ok(Self {
            shared,
            stop,
            thread: Some((thread, ended)),
            spawned,
        })
    }

    /// Replaces the answer for that device and PSM, or adds one.
    pub fn route(&self, device: BdAddr, psm: u16, answer: Answer) {
        self.shared.update(|state| {
            match state
                .routes
                .iter_mut()
                .find(|route| route.device == device && route.psm == psm)
            {
                Some(route) => route.answer = answer,
                None => state.routes.push(Route {
                    device,
                    psm,
                    answer,
                }),
            }
        });
    }

    pub fn unroute(&self, device: BdAddr, psm: u16) {
        self.shared.update(|state| {
            state
                .routes
                .retain(|route| route.device != device || route.psm != psm);
        });
    }

    /// Every trapped L2CAP connect so far, oldest first.
    #[must_use]
    pub fn dials(&self) -> Vec<Dial> {
        self.shared.lock().dials.clone()
    }

    /// Every trapped Bluetooth socket call so far, served or refused.
    #[must_use]
    pub fn sockets(&self) -> usize {
        self.shared.lock().sockets
    }

    /// Waits until at least `count` dials were trapped and returns them all.
    pub fn wait_dials(&self, count: usize, deadline: Duration) -> Result<Vec<Dial>, TrapError> {
        let started = Instant::now();
        let (state, _) = self
            .shared
            .changed
            .wait_timeout_while(self.shared.lock(), deadline, |state| {
                state.dials.len() < count && state.failure.is_none()
            })
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(failure) = &state.failure {
            return Err(TrapError::Supervisor(failure.clone()));
        }
        if state.dials.len() >= count {
            return Ok(state.dials.clone());
        }
        Err(TrapError::Deadline {
            waited: started.elapsed(),
            wanted: count,
            dials: state.dials.clone(),
            spawned: self.spawned,
        })
    }

    /// Stops the thread and returns every dial; `Err` when the thread failed or the child made a call the trap refused.
    pub fn finish(mut self) -> Result<Vec<Dial>, TrapError> {
        self.stop();
        let state = self.shared.lock();
        if let Some(failure) = &state.failure {
            return Err(TrapError::Supervisor(failure.clone()));
        }
        if !state.violations.is_empty() {
            return Err(TrapError::Violations {
                violations: state.violations.clone(),
                dials: state.dials.clone(),
                spawned: self.spawned,
            });
        }
        Ok(state.dials.clone())
    }

    fn stop(&mut self) {
        let Some((thread, ended)) = self.thread.take() else {
            return;
        };
        #[expect(
            clippy::host_endian_bytes,
            reason = "an eventfd reads its counter in host order"
        )]
        let one = 1_u64.to_ne_bytes();
        let failure = match rustix::io::write(&*self.stop, &one) {
            Err(errno) => Some(format!("eventfd write failed: {errno}")),
            Ok(_) => match ended.recv_timeout(STOP_WAIT) {
                Ok(never) => match never {},
                Err(RecvTimeoutError::Disconnected) => thread
                    .join()
                    .err()
                    .map(|_| "the trap thread panicked inside serve".to_owned()),
                Err(RecvTimeoutError::Timeout) => Some(format!(
                    "the trap thread did not stop within {} ms; serve must hand the channel on and return",
                    STOP_WAIT.as_millis()
                )),
            },
        };
        if let Some(failure) = failure {
            self.shared.update(|state| {
                state.failure.get_or_insert(failure);
            });
        }
    }
}

impl Drop for Trap {
    fn drop(&mut self) {
        self.stop();
    }
}

impl fmt::Debug for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Trap")
            .field("spawned", &self.spawned)
            .finish_non_exhaustive()
    }
}
