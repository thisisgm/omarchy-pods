//! Support tests: the driver under a queue that closes.

use std::sync::Arc;
use std::time::Duration;

use pods_sys::l2cap::ChannelError;
use rustix::io::Errno;
use rustix::time::{ClockId, clock_gettime};
use tokio::runtime::Builder;
use tokio::sync::mpsc;
use tokio::time::sleep;

use super::super::{Connector, REQUEST_QUEUE, drive, queue};
use super::report;
use crate::daemon::hub::Hub;

/// How long a driver with a closed queue is watched for spinning or ending.
const IDLE_WATCH: Duration = Duration::from_millis(200);

/// CPU a thread that only sleeps through `IDLE_WATCH` stays under; a spinning driver takes all of it.
const IDLE_CPU: Duration = Duration::from_millis(50);

fn thread_cpu() -> Duration {
    let spent = clock_gettime(ClockId::ThreadCPUTime);
    let seconds = u64::try_from(spent.tv_sec).unwrap();
    let nanos = u32::try_from(spent.tv_nsec).unwrap();
    Duration::new(seconds, nanos)
}

#[test]
fn a_closed_queue_neither_ends_nor_spins_the_driver() {
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let (link, inputs) = queue();
        // Only a test can close the queue: the hub and the feedback here hold another queue's senders.
        let (elsewhere, _unread_elsewhere) = queue();
        let (requests, _unread) = mpsc::channel(REQUEST_QUEUE);
        let refuse: Connector = Arc::new(|_| Err(ChannelError::Connect(Errno::HOSTDOWN)));
        let hub = Hub::new(elsewhere.clone());
        let driver = tokio::spawn(drive(hub, inputs, elsewhere, requests, refuse));
        drop(link);
        let before = thread_cpu();
        sleep(IDLE_WATCH).await;
        let spent = thread_cpu().saturating_sub(before);
        report(&format!("the driver took {spent:?} of CPU in {IDLE_WATCH:?}"));
        assert!(!driver.is_finished(), "the driver ended when its queue closed");
        assert!(spent < IDLE_CPU, "the driver spun on its closed queue");
        driver.abort();
    });
}
