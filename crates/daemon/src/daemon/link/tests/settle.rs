//! The resume settle on the driver's paused clock: each wake's dial and sweep come exactly the settle after the wake.

use std::sync::Arc;
use std::time::Duration;

use pods_core::link::ladder::HELD_DELAYS_MS;
use pods_core::policy::SleepEdge;
use pods_sys::l2cap::ChannelError;
use rustix::io::Errno;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::super::{Connector, Input, Request};
use super::{Running, device_up};

/// `RESUME_SETTLE_MS`, written out so a drifted settle goes red.
const SETTLE: Duration = Duration::from_millis(2000);

/// The dials of a climb that stops on a held rung of 8 s, far past the settle.
const DIALS_INTO_BACKOFF: usize = 4;

/// The first dial and one per held rung; the next waits out the parked retry.
const DIALS_TO_PARKED: usize = HELD_DELAYS_MS.len().saturating_add(1);

/// A driver whose every dial is refused, and the paused time each refusal reaches it.
fn refusing() -> (Running, mpsc::UnboundedReceiver<Instant>) {
    let refuse: Connector = Arc::new(|_| Err(ChannelError::Connect(Errno::HOSTDOWN)));
    let (stamps, stamped) = mpsc::unbounded_channel();
    let running = Running::paused(
        refuse,
        Box::new(move |_| {
            // A test that has its dials no longer listens.
            stamps.send(Instant::now()).ok();
        }),
    );
    (running, stamped)
}

/// logind's edges into sleep and out; gives the paused time of the wake.
async fn sleep_and_wake(inputs: &mpsc::Sender<Input>) -> Instant {
    inputs
        .send(Input::Sleep(SleepEdge::Sleeping))
        .await
        .unwrap();
    inputs.send(Input::Sleep(SleepEdge::Woke)).await.unwrap();
    Instant::now()
}

/// Lets `dials` refusals by, then gives the gap from a wake to the dial after it.
fn dial_after_a_wake(dials: usize) -> Duration {
    let (running, mut stamped) = refusing();
    let inputs = running.inputs().clone();
    device_up(&inputs);
    running.script(async move {
        for _ in 0..dials {
            stamped.recv().await.unwrap();
        }
        let woke = sleep_and_wake(&inputs).await;
        stamped.recv().await.unwrap().duration_since(woke)
    })
}

#[test]
fn a_wake_in_backoff_dials_the_settle_after_it() {
    assert_eq!(dial_after_a_wake(DIALS_INTO_BACKOFF), SETTLE);
}

#[test]
fn a_wake_retries_parked_pods_the_settle_after_it() {
    assert_eq!(dial_after_a_wake(DIALS_TO_PARKED), SETTLE);
}

#[test]
fn a_wake_sweeps_bluez_the_settle_after_it() {
    let (mut running, _stamped) = refusing();
    let inputs = running.inputs().clone();
    let mut requests = running.requests();
    let gap = running.script(async move {
        let woke = sleep_and_wake(&inputs).await;
        let swept = requests.recv().await.unwrap();
        assert!(matches!(swept, Request::Sweep), "{swept:?}");
        Instant::now().duration_since(woke)
    });
    assert_eq!(gap, SETTLE);
}
