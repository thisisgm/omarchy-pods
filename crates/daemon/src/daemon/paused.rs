//! Test support: a future run on tokio's paused clock, where a wait that never ends fails instead of parking the test.

use std::future::Future;
use std::time::Duration;

use tokio::runtime::Builder;
use tokio::time::timeout;

/// Bounds a whole test's paused time, twice the most any test here spends (the 30 s BlueZ call); it costs nothing.
const PAUSED_HANG_LIMIT: Duration = Duration::from_secs(60);

/// Runs `work` to its end on a current thread runtime whose clock starts paused.
pub(in crate::daemon) fn on_paused_clock<F: Future>(work: F) -> F::Output {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    runtime
        .block_on(async { timeout(PAUSED_HANG_LIMIT, work).await })
        .unwrap_or_else(|_| {
            panic!("the work did not end within {PAUSED_HANG_LIMIT:?} of paused time")
        })
}
