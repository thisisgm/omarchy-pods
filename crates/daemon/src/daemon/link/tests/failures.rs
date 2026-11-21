//! Support tests: the driver under a queue that never empties, a write that fails, and a queue that closes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc as std_mpsc};
use std::thread;
use std::time::{Duration, Instant};

use pods_core::command::{AdaptiveLevel, Command, CommandError, Outcome};
use pods_core::link::ladder::HANDSHAKE_STEP_TIMEOUT_MS;
use pods_fake::pair;
use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::noise::NoiseMode;
use pods_samples::{NOISE, setting};
use pods_sys::l2cap::{Channel, ChannelError, Received};
use rustix::io::Errno;
use rustix::time::{ClockId, clock_gettime};
use tokio::runtime::Builder;
use tokio::sync::mpsc;
use tokio::time::sleep;

use super::super::reader::RECV_BUFFER_LEN;
use super::super::{Connector, Input, REQUEST_QUEUE, drive, queue};
use super::{
    HANG_LIMIT, REAL_TIME_SLACK, Running, block_on, device_up, report, wait_for_state, wire,
};
use crate::daemon::hub::Hub;

/// Writes into an unread socket before one must have failed; a few hundred fill it.
const SENDS_MAX: usize = 10_000;

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

fn level_frame(level: AdaptiveLevel) -> Vec<u8> {
    let frame = ControlCommand {
        id: ControlId::ADAPTIVE_NOISE_LEVEL,
        value: [level.percent(), 0, 0, 0],
    };
    frame.encode()
}

/// Every SDU left on `pods_end` until the host's close shows.
fn drain(pods_end: &Channel) -> Vec<Vec<u8>> {
    pods_end.set_recv_timeout(HANG_LIMIT).unwrap();
    let mut buf = vec![0_u8; RECV_BUFFER_LEN];
    let mut sdus = Vec::new();
    loop {
        match pods_end.recv(&mut buf).unwrap() {
            Received::Sdu(len) => sdus.push(buf[..len].to_vec()),
            Received::Closed => return sdus,
            Received::Truncated { full_len } => panic!("the host sent a {full_len} byte SDU"),
            Received::TimedOut => panic!("no close from the host within {HANG_LIMIT:?}"),
        }
    }
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
fn a_failed_write_is_refused_and_never_known() {
    let (running, pods_end, _) = hand_held();
    let hub = running.hub();
    for sdu in [
        pods_samples::handshake_ack(),
        pods_samples::features_ack(),
        pods_samples::metadata(),
        setting(NOISE, wire(NoiseMode::Adaptive)),
    ] {
        pods_end.send(&sdu).unwrap();
    }
    wait_for_state(&hub, HANG_LIMIT, "adaptive pods", |state| {
        state.connected && state.noise_mode == NoiseMode::Adaptive
    });
    let levels = [
        AdaptiveLevel::new(10).unwrap(),
        AdaptiveLevel::new(20).unwrap(),
    ];
    // Nothing reads the pods' end, so the socket's queue fills and a write fails.
    let mut sent = 0;
    let (failed, refusal) = loop {
        assert!(sent < SENDS_MAX, "{sent} writes and none failed");
        let level = levels[sent % levels.len()];
        let outcome = block_on(hub.submit(Command::SetAdaptiveLevel(level)));
        if outcome != Outcome::Sent {
            break (level, outcome);
        }
        sent += 1;
    };
    report(&format!("a write failed after {sent} were sent"));
    assert_eq!(refusal, Outcome::Refused(CommandError::NotConnected));
    let repeat = block_on(hub.submit(Command::SetAdaptiveLevel(failed)));
    assert!(
        !matches!(repeat, Outcome::Sent | Outcome::Confirmed),
        "the failed level asked again: {repeat:?}"
    );
    wait_for_state(&hub, HANG_LIMIT, "the link down", |state| !state.connected);
    let written = levels.map(level_frame);
    let heard = drain(&pods_end)
        .iter()
        .filter(|sdu| written.contains(sdu))
        .count();
    assert_eq!(
        heard, sent,
        "levels the pods hold against levels answered sent"
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
