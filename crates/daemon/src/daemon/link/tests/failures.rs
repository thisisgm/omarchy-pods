//! Support tests: a channel that never empties, a channel the peer closes, a write that fails, a queue that closes, and a full request queue at an advert monitor edge.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc as std_mpsc};
use std::thread;
use std::time::{Duration, Instant};

use pods_core::command::{AdaptiveLevel, Command, CommandError, Outcome};
use pods_core::link::ladder::HANDSHAKE_STEP_TIMEOUT_MS;
use pods_core::state::State;
use pods_fake::pair;
use pods_proto::keys::{Key, ProximityKeys};
use pods_proto::noise::NoiseMode;
use pods_samples::{ADAPTIVE_LEVEL, IDENTITY_KEY, NOISE, PAYLOAD_KEY, setting};
use pods_sys::l2cap::{Channel, ChannelError, Received};
use rustix::io::Errno;
use rustix::time::{ClockId, clock_gettime};
use tokio::runtime::Builder;
use tokio::sync::{mpsc, watch};
use tokio::task::yield_now;
use tokio::time::{sleep, timeout};

use super::super::reader::{Open, RECV_BUFFER_LEN};
use super::super::{
    Connector, Input, Outlets, REQUEST_QUEUE, Request, Turn, drive, next_of, queue,
};
use super::{
    HANG_LIMIT, REAL_TIME_SLACK, Running, block_on, device_up, report, unread_outlets,
    wait_for_state, wire,
};
use crate::daemon::hub::Hub;

/// Writes into an unread socket before one must have failed; a few hundred fill it.
const SENDS_MAX: usize = 10_000;

/// Threads writing to the channel at once, so it never empties between two reads.
const FLOODERS: usize = 4;

/// Turns each of the channel and the queue gets in the turn taking test.
const TURNS_EACH: usize = 3;

const IDLE_WATCH: Duration = Duration::from_millis(200);

/// A sleeping thread stays under this through `IDLE_WATCH`; a spinning driver takes it all.
const IDLE_CPU: Duration = Duration::from_millis(50);

fn hand_held() -> (Running, Channel, Instant) {
    let (far_ends, far) = std_mpsc::channel();
    let connector: Connector = Arc::new(move |_| {
        let (host_end, pods_end) = pair();
        // Only the first connect reaches the test; a later retry finds the receiver gone and fails.
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
    // The handshake is never answered, and a packet it ignores keeps the channel readable.
    let flood = pods_samples::battery();
    let stop = AtomicBool::new(false);
    let seen = thread::scope(|scope| {
        for _ in 0..FLOODERS {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    match pods_end.try_send(&flood) {
                        Ok(()) => {}
                        // Never blocks, so the stop ends a flooder whatever the driver does.
                        Err(ChannelError::Send(Errno::AGAIN)) => thread::yield_now(),
                        Err(_) => return,
                    }
                }
            });
        }
        let seen = loop {
            match pods_end.recv(&mut buf) {
                Ok(Received::Closed) => break Ok(connected.elapsed()),
                Ok(Received::Sdu(_) | Received::Truncated { .. }) => {}
                Ok(Received::TimedOut) => {
                    break Err(format!("still in the handshake after {HANG_LIMIT:?}"));
                }
                Err(error) => break Err(format!("recv on the pods end failed: {error}")),
            }
        };
        stop.store(true, Ordering::Relaxed);
        seen
    });
    let closed = seen.unwrap_or_else(|why| panic!("{why}"));
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

/// A watched channel that holds `waiting`, and the pods' end, which must outlive it; the poller has told of every SDU in its one edge, so nothing wakes a read that has cleared its readiness.
async fn readable_channel(waiting: &[Vec<u8>]) -> (Open, Channel) {
    let (host_end, pods_end) = pair();
    let mut open = Open::watch(host_end).unwrap();
    pods_end.send(&pods_samples::battery()).unwrap();
    for sdu in waiting {
        pods_end.send(sdu).unwrap();
    }
    assert!(open.read().await.is_some());
    (open, pods_end)
}

/// Which of the channel and the queue served each of `count` waits, the channel asked first in the first.
async fn served(
    inputs: &mut mpsc::Receiver<Input>,
    open: &mut Open,
    count: usize,
) -> Vec<&'static str> {
    let mut channel_first = false;
    let mut order = Vec::new();
    for _ in 0..count {
        let next = next_of(inputs, Some(open), &mut channel_first);
        order.push(match timeout(HANG_LIMIT, next).await.unwrap() {
            Turn::Read(Some(_)) => "channel",
            Turn::Input(_) => "queue",
            Turn::Read(None) | Turn::Tick => "neither",
        });
    }
    order
}

#[test]
fn the_channel_and_the_queue_take_turns_while_both_are_ready() {
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    let order = runtime.block_on(async {
        let (link, mut inputs) = queue();
        let waiting = vec![pods_samples::battery(); TURNS_EACH];
        let (mut open, _pods_end) = readable_channel(&waiting).await;
        for _ in 0..TURNS_EACH {
            link.send(Input::SendFailed).await.unwrap();
        }
        served(&mut inputs, &mut open, TURNS_EACH * 2).await
    });
    assert_eq!(order, ["channel", "queue"].repeat(TURNS_EACH));
}

#[test]
fn an_oversized_sdu_ends_the_channels_turn_and_costs_no_other() {
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    let order = runtime.block_on(async {
        let (link, mut inputs) = queue();
        let waiting = [vec![0; RECV_BUFFER_LEN + 1], pods_samples::battery()];
        let (mut open, _pods_end) = readable_channel(&waiting).await;
        link.send(Input::SendFailed).await.unwrap();
        served(&mut inputs, &mut open, 2).await
    });
    // The channel is asked first and drops the oversized SDU, the queue is served, and the SDU behind it comes next.
    assert_eq!(order, ["queue", "channel"]);
}

#[test]
fn a_peer_close_ends_the_read_after_the_sdu_sent_before_it() {
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    let read = runtime.block_on(async {
        let (mut open, pods_end) = readable_channel(&[pods_samples::battery()]).await;
        drop(pods_end);
        let sent_before = timeout(HANG_LIMIT, open.read()).await.unwrap();
        let end = timeout(HANG_LIMIT, open.read()).await.unwrap();
        (sent_before.map(|sdu| sdu.to_vec()), end.is_none())
    });
    assert_eq!(read, (Some(pods_samples::battery()), true));
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
    // Read before the repeat, which stores the level while no link is up, as the C++ daemon does.
    assert_ne!(
        hub.state().adaptive_noise_level,
        failed,
        "the level whose write failed was stored"
    );
    let repeat = block_on(hub.submit(Command::SetAdaptiveLevel(failed)));
    assert!(
        !matches!(repeat, Outcome::Sent | Outcome::Confirmed),
        "the failed level asked again: {repeat:?}"
    );
    wait_for_state(&hub, HANG_LIMIT, "the link down", |state| !state.connected);
    let written = levels.map(|level| setting(ADAPTIVE_LEVEL, level.percent()));
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
        // Only a test can close the queue: the hub and feedback here hold another queue's senders.
        let (elsewhere, _unread_elsewhere) = queue();
        let (requests, _unread) = mpsc::channel(REQUEST_QUEUE);
        let refuse: Connector = Arc::new(|_| Err(ChannelError::Connect(Errno::HOSTDOWN)));
        let hub = Hub::new(elsewhere.clone(), State::default());
        let driver = tokio::spawn(drive(
            hub,
            inputs,
            elsewhere,
            requests,
            refuse,
            unread_outlets(),
            None,
        ));
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

#[test]
fn the_monitor_wanted_at_start_reaches_the_watcher_behind_a_full_request_queue() {
    block_on(async {
        let (link, inputs) = queue();
        let (requests, mut watcher) = mpsc::channel(REQUEST_QUEUE);
        for _ in 0..REQUEST_QUEUE {
            requests.try_send(Request::Sweep).unwrap();
        }
        let refuse: Connector = Arc::new(|_| Err(ChannelError::Connect(Errno::HOSTDOWN)));
        let hub = Hub::new(link.clone(), State::default());
        let keys = ProximityKeys {
            identity: Key::from_bytes(IDENTITY_KEY),
            payload: Key::from_bytes(PAYLOAD_KEY),
        };
        let (monitor, mut wanted) = watch::channel(false);
        let outlets = Outlets {
            monitor,
            ..unread_outlets()
        };
        let driver = tokio::spawn(drive(
            hub,
            inputs,
            link,
            requests,
            refuse,
            outlets,
            Some(keys),
        ));
        // The driver routes the policy's start before its first wait.
        yield_now().await;
        assert!(wanted.has_changed().unwrap());
        assert!(*wanted.borrow_and_update());
        let mut swept = 0_usize;
        while let Ok(Request::Sweep) = watcher.try_recv() {
            swept = swept.saturating_add(1);
        }
        assert_eq!(swept, REQUEST_QUEUE, "the queue stayed full");
        driver.abort();
    });
}
