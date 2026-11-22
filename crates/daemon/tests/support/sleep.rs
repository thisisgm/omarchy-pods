//! A suspend and a wake through the fake logind, each edge confirmed in the daemon's log before the test goes on.

use std::time::{Duration, Instant};

use super::rig::Rig;
use super::wait_for;

/// `RESUME_SETTLE_MS` of the daemon's policy: the wait after a wake before it touches Bluetooth again.
pub(crate) const RESUME_SETTLE: Duration = Duration::from_millis(2000);

/// `SYSTEM_BUS_RETRY_MS` of the daemon's watchers: the pause before a lost system bus is dialled again.
pub(crate) const SYSTEM_BUS_RETRY: Duration = Duration::from_millis(5000);

/// Logged once the daemon's match rule for logind's signal is in place.
pub(crate) const WATCHING: &str = "sleep: watching logind for a suspend";
pub(crate) const SLEEPING: &str = "sleep: the system is going to sleep";
pub(crate) const WOKE: &str = "sleep: the system woke";

/// logind's edge into sleep, once the daemon has queued it for the driver.
pub(crate) fn sleep(rig: &mut Rig) {
    edge(rig, true, SLEEPING);
}

/// logind's wake edge, once the daemon has queued it; gives the moment just before the edge went out.
pub(crate) fn wake(rig: &mut Rig) -> Instant {
    edge(rig, false, WOKE)
}

/// Waits for the grace of a lost link, entered from `closing` or from `backoff` by which of BlueZ's word and the hang up came first.
pub(crate) fn wait_grace(rig: &mut Rig) {
    let daemon = rig.daemon();
    wait_for("the link to enter its grace", || {
        let log = daemon.log();
        let mut steps = log.lines().filter(|line| line.starts_with("link: "));
        steps.any(|line| line.ends_with(" to grace")).then_some(())
    });
}

/// How many lines of the daemon's log equal `line`.
pub(crate) fn logged(rig: &mut Rig, line: &str) -> usize {
    rig.daemon()
        .log()
        .lines()
        .filter(|logged| *logged == line)
        .count()
}

/// Sends one edge and gives the moment just before it went out.
fn edge(rig: &mut Rig, going_down: bool, line: &str) -> Instant {
    rig.wait_logged(WATCHING);
    let before = logged(rig, line);
    let sent = Instant::now();
    rig.logind().prepare_for_sleep(going_down);
    wait_for(&format!("the daemon to log `{line}` once more"), || {
        (logged(rig, line) > before).then_some(())
    });
    sent
}
