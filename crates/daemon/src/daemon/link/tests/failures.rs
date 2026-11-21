//! Support tests: the driver under a queue that never empties, and a queue that closes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc as std_mpsc};
use std::thread;
use std::time::{Duration, Instant};

use pods_core::link::ladder::HANDSHAKE_STEP_TIMEOUT_MS;
use pods_fake::pair;
use pods_sys::l2cap::{Channel, ChannelError, Received};
use rustix::io::Errno;
use rustix::time::{ClockId, clock_gettime};
use tokio::runtime::Builder;
use tokio::sync::mpsc;
use tokio::time::sleep;

use super::super::reader::RECV_BUFFER_LEN;
use super::super::{Connector, Input, REQUEST_QUEUE, drive, queue};
use super::{HANG_LIMIT, REAL_TIME_SLACK, Running, device_up, report};
use crate::daemon::hub::Hub;

/// Threads feeding the queue at once, so it never empties between two inputs.
const FLOODERS: usize = 4;

/// How long a driver with a closed queue is watched for spinning or ending.
const IDLE_WATCH: Duration = Duration::from_millis(200);

/// CPU a thread that only sleeps through `IDLE_WATCH` stays under; a spinning driver takes all of it.
const IDLE_CPU: Duration = Duration::from_millis(50);

/// A driver whose first channel's pods end comes back to the test unread, with the time its connect returned.
fn hand_held() -> (Running, Channel, Instant) {
    let (far_ends, far) = std_mpsc::channel();
    let connector: Connector = Arc::new(move |_| {
        let (host_end, pods_end) = pair();
        // Only the first connect reaches the test; a retry after it finds the receiver gone and fails.
        match far_ends.send((pods_end, Instant::now())) {
            Ok(()) => Ok(host_end),
            Err(_) => Err(ChannelError::Connect(Errno::HOSTDOWN)),
        }
    });
    let running = Running::start(connector, None);
    device_up(running.inputs());
    let (pods_end, connected) = far.recv_timeout(HANG_LIMIT).unwrap();
    (running, pods_end, connected)
}

fn thread_cpu() -> Duration {
    let spent = clock_gettime(ClockId::ThreadCPUTime);
    let seconds = u64::try_from(spent.tv_sec).unwrap();
    let nanos = u32::try_from(spent.tv_nsec).unwrap();
    Duration::new(seconds, nanos)
}

#[test]
fn packets_never_hold_back_a_passed_deadline() {
    let (running, pods_end, connected) = hand_held();
    pods_end.set_recv_timeout(HANG_LIMIT).unwrap();
    let mut buf = vec![0_u8; RECV_BUFFER_LEN];
    assert!(matches!(pods_end.recv(&mut buf), Ok(Received::Sdu(_))));
    // The handshake request is never answered, and a packet the handshake ignores keeps the queue full.
    let flood = pods_samples::battery();
    let inputs = running.inputs();
    let stop = AtomicBool::new(false);
    let seen = thread::scope(|scope| {
        for _ in 0..FLOODERS {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let sdu = Input::Sdu(flood.clone().into_boxed_slice());
                    if inputs.blocking_send(sdu).is_err() {
                        return;
                    }
                }
            });
        }
        let seen = loop {
            match pods_end.recv(&mut buf) {
                Ok(Received::Closed) => break Some(connected.elapsed()),
                Ok(Received::Sdu(_) | Received::Truncated { .. }) => {}
                Ok(Received::TimedOut) | Err(_) => break None,
            }
        };
        stop.store(true, Ordering::Relaxed);
        seen
    });
    let closed = seen.unwrap_or_else(|| panic!("still in the handshake after {HANG_LIMIT:?}"));
    let delay = Duration::from_millis(HANDSHAKE_STEP_TIMEOUT_MS);
    report(&format!(
        "the unanswered handshake closed {closed:?} after the connect"
    ));
    assert!(
        closed >= delay && closed <= delay + REAL_TIME_SLACK,
        "{closed:?}, from {delay:?} to {:?}",
        delay + REAL_TIME_SLACK
    );
    drop(running);
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
        report(&format!(
            "the driver took {spent:?} of CPU in {IDLE_WATCH:?}"
        ));
        assert!(
            !driver.is_finished(),
            "the driver ended when its queue closed"
        );
        assert!(spent < IDLE_CPU, "the driver spun on its closed queue");
        driver.abort();
    });
}
