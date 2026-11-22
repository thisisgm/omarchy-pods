//! Support tests: the driver in process; each behaviour's proof is a test against the binary.

use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc as std_mpsc};
use std::thread;
use std::time::{Duration, Instant};

use pods_core::command::{Command, CommandError, NoiseTarget, Outcome};
use pods_core::link::End;
use pods_core::link::ladder::{FIRST_RETRY_MS, HELD_DELAYS_MS};
use pods_core::state::State;
use pods_fake::{Fault, Pods, Profile, pair};
use pods_proto::model::Model;
use pods_proto::noise::NoiseMode;
use pods_samples::{CASE, Cell, NOISE, RIGHT, host, setting};
use pods_sys::l2cap::{Channel, ChannelError};
use rustix::io::Errno;
use tokio::runtime::Builder;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

use super::super::reader::{self, RECV_BUFFER_LEN};
use super::super::{Connector, DeviceEvent, INPUT_QUEUE, Input, REQUEST_QUEUE, STOP_WAIT_MS};
use super::super::{drive, queue, stop};
use super::{
    HANG_LIMIT, REAL_TIME_SLACK, Running, block_on, device, device_up, report, unread_outlets,
    wait_for_state, wait_for_state_async, wire,
};
use crate::daemon::SUBSCRIBER_BACKLOG;
use crate::daemon::hub::Hub;

const POLL: Duration = Duration::from_millis(5);

const ROUNDS: usize = 200;

/// Hang-up offsets around a submit: a set confirms in about 50 us, so both orders come up.
const RACE_SPREAD_US: usize = 200;

/// The most a set may take from submit to the pods hearing it.
const TURN_BOUND: Duration = Duration::from_millis(20);

/// A battery none of the first burst carries, so its arrival shows every earlier packet was taken.
const SENTINEL: Cell = Cell {
    level: 13,
    charging: true,
};

/// The first rungs of the held ladder, 1 + 2 + 4 s apart; the rest is the pure machine's to prove.
const LADDER_ATTEMPTS: usize = 4;

/// Finer than a sleep can wake.
fn spin(duration: Duration) {
    let started = Instant::now();
    while started.elapsed() < duration {
        std::hint::spin_loop();
    }
}

fn fake_connector(pods: &Pods) -> (Connector, std_mpsc::Receiver<Instant>) {
    let (connected, connects) = std_mpsc::channel();
    let pods = pods.clone();
    let connector: Connector = Arc::new(move |_| {
        let (host_end, pods_end) = pair();
        pods.serve(pods_end);
        // A test that stopped listening no longer counts connects.
        connected.send(Instant::now()).ok();
        Ok(host_end)
    });
    (connector, connects)
}

fn filled(state: &State) -> bool {
    state.connected
        && state.model == Model::AirPodsPro3
        && state.batteries.case.is_some()
        && state.ear.any_in_ear()
}

fn link_up(pods: &Pods) -> (Running, std_mpsc::Receiver<Instant>) {
    let (connector, connects) = fake_connector(pods);
    let running = Running::start(connector, None);
    device_up(running.inputs());
    wait_for_state(&running.hub(), HANG_LIMIT, "the first burst", filled);
    (running, connects)
}

/// The state shows `sentinel` only after everything queued before it.
fn wait_for_battery(pods: &Pods, hub: &Hub, sentinel: Cell) -> State {
    pods.set_battery(sentinel, RIGHT, CASE);
    wait_for_state(hub, HANG_LIMIT, "the sentinel battery", |state| {
        state.batteries.left.map(|left| left.level) == Some(sentinel.level)
    })
}

fn tasks() -> usize {
    fs::read_dir("/proc/self/task").unwrap().count()
}

async fn wait_for_tasks(count: usize, within: Duration) -> Duration {
    let started = Instant::now();
    loop {
        let now = tasks();
        if now == count {
            return started.elapsed();
        }
        assert!(
            started.elapsed() < within,
            "{now} threads after {within:?}, {count} before the open"
        );
        sleep(POLL).await;
    }
}

fn noise(mode: NoiseMode) -> Command {
    Command::SetNoise(NoiseTarget::Mode(mode))
}

/// Alternates between the two modes every model offers.
fn noise_for(round: usize) -> (NoiseMode, u8) {
    let mode = if round.is_multiple_of(2) {
        NoiseMode::Anc
    } else {
        NoiseMode::Transparency
    };
    (mode, wire(mode))
}

fn range(durations: &[Duration]) -> (Duration, Duration) {
    let min = durations.iter().min().copied().unwrap_or_default();
    let max = durations.iter().max().copied().unwrap_or_default();
    (min, max)
}

/// Deterministic, so a failing run repeats; xorshift64.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(
            self.next()
                .checked_rem(u64::try_from(bound).unwrap())
                .unwrap(),
        )
        .unwrap()
    }
}

#[test]
fn handshake_runs_against_the_fake_pods_and_the_state_fills() {
    let pods = Pods::new(Profile::default());
    let (running, _connects) = link_up(&pods);
    pods.wait_heard(&host::request_notifications(), 1, HANG_LIMIT)
        .unwrap();
    let heard: Vec<Vec<u8>> = pods.heard().into_iter().map(|heard| heard.sdu).collect();
    let opening = [
        host::handshake(),
        host::set_specific_features(),
        host::request_notifications(),
    ];
    assert_eq!(heard.get(..3), Some(&opening[..]));
    let state = running.hub().state();
    assert!(state.connected);
    assert_eq!(state.device_name, "Test\u{2019}s AirPods Pro");
    assert_eq!(state.model_number, "A3063");
    assert_eq!(state.model, Model::AirPodsPro3);
    let batteries = state.batteries;
    assert!(batteries.left.is_some() && batteries.right.is_some() && batteries.case.is_some());
    assert!(state.ear.any_in_ear());
    assert_eq!(state.noise_mode, NoiseMode::Off);
    drop(running);
    pods.finish().unwrap();
}

#[test]
fn burst_of_38_packets_reaches_a_healthy_subscriber_in_order() {
    let pods = Pods::new(Profile::default());
    let (connector, _connects) = fake_connector(&pods);
    let running = Running::start(connector, None);
    let hub = running.hub();
    let (first, mut changes) = hub.follow();
    let drainer = thread::spawn(move || {
        let mut seen = Vec::new();
        block_on(async {
            while seen.last().is_none_or(|state| !filled(state)) {
                let next = timeout(HANG_LIMIT, changes.recv()).await;
                seen.push(next.unwrap().expect("the subscriber was closed"));
            }
        });
        (seen, changes)
    });
    device_up(running.inputs());
    let (seen, mut changes) = drainer.join().unwrap();
    let mut before = &first;
    for state in &seen {
        assert_ne!(state, before, "a change equal to the one before it");
        before = state;
    }
    let first_with = |has: fn(&State) -> bool| seen.iter().position(has).unwrap();
    let battery = first_with(|state| state.batteries.case.is_some());
    let ear = first_with(|state| state.ear.any_in_ear());
    let metadata = first_with(|state| state.model == Model::AirPodsPro3);
    assert!(
        battery < ear && ear < metadata,
        "{battery}, {ear}, {metadata}"
    );
    pods.wait_heard(&host::request_notifications(), 1, HANG_LIMIT)
        .unwrap();
    let last = wait_for_battery(&pods, &hub, SENTINEL);
    let after_sentinel = block_on(async {
        loop {
            let change = timeout(HANG_LIMIT, changes.recv()).await.unwrap();
            let change = change.expect("the subscriber was closed");
            if change == last {
                return change;
            }
        }
    });
    assert_eq!(after_sentinel, hub.state());
    drop(running);
    pods.finish().unwrap();
}

#[test]
fn a_subscriber_that_never_reads_is_dropped_and_get_still_answers() {
    let pods = Pods::new(Profile::default());
    let (running, _connects) = link_up(&pods);
    let hub = running.hub();
    let (_, mut unread) = hub.follow();
    for round in 0..ROUNDS {
        pods.set_setting(NOISE, noise_for(round).1);
    }
    let state = wait_for_battery(&pods, &hub, SENTINEL);
    assert_eq!(state.noise_mode, NoiseMode::Transparency, "the last report");
    assert_eq!(hub.state(), state);
    let mut held = 0;
    while unread.try_recv().is_ok() {
        held += 1;
    }
    assert_eq!(held, SUBSCRIBER_BACKLOG);
    assert!(unread.is_closed(), "the subscriber nobody read");
    drop(running);
    pods.finish().unwrap();
}

#[test]
fn peer_hang_up_mid_burst_starts_the_held_ladder() {
    let pods = Pods::new(Profile::default());
    pods.fault(Fault::CloseAfter(10));
    let (connector, connects) = fake_connector(&pods);
    let running = Running::start(connector, None);
    let hub = running.hub();
    let (_, mut changes) = hub.follow();
    device_up(running.inputs());
    connects.recv_timeout(HANG_LIMIT).unwrap();
    let closed = pods.wait_closed(1, HANG_LIMIT).unwrap()[0];
    block_on(async {
        let mut was_up = false;
        loop {
            let change = timeout(HANG_LIMIT, changes.recv()).await.unwrap().unwrap();
            was_up |= change.connected;
            if was_up && !change.connected {
                return;
            }
        }
    });
    let down_after = closed.elapsed();
    assert!(
        down_after <= Duration::from_secs(1),
        "connected went false {down_after:?} after the hang-up"
    );
    let retried = connects.recv_timeout(HANG_LIMIT).unwrap() - closed;
    let first_retry = Duration::from_millis(FIRST_RETRY_MS);
    report(&format!(
        "the second connect came {retried:?} after the hang-up"
    ));
    assert!(
        retried >= first_retry && retried <= first_retry + REAL_TIME_SLACK,
        "{retried:?}, from {first_retry:?} to {:?}",
        first_retry + REAL_TIME_SLACK
    );
    drop(running);
    pods.finish().unwrap();
}

#[test]
fn set_racing_a_hang_up_always_resolves() {
    let pods = Pods::new(Profile::default());
    let (running, _connects) = link_up(&pods);
    let hub = running.hub();
    let mut noise_source = Noise(0x9e37_79b9_7f4a_7c15);
    let mut waits = Vec::with_capacity(ROUNDS);
    let mut outcomes = BTreeMap::new();
    for round in 0..ROUNDS {
        wait_for_state(&hub, HANG_LIMIT, "the link up again", |state| {
            state.connected
        });
        let offset =
            Duration::from_micros(u64::try_from(noise_source.below(RACE_SPREAD_US)).unwrap());
        let hang_up_first = round.is_multiple_of(2);
        let (mode, _) = noise_for(round);
        let (outcome, waited) = thread::scope(|scope| {
            scope.spawn(|| {
                if !hang_up_first {
                    spin(offset);
                }
                pods.hang_up();
            });
            if hang_up_first {
                spin(offset);
            }
            let started = Instant::now();
            let submitted = hub.submit(noise(mode));
            let outcome = block_on(async { timeout(Duration::from_secs(2), submitted).await });
            (outcome, started.elapsed())
        });
        let outcome = outcome.unwrap_or_else(|_| panic!("round {round}: no outcome within 2 s"));
        assert!(
            matches!(
                outcome,
                Outcome::Confirmed
                    | Outcome::Refused(CommandError::NotConnected | CommandError::Timeout)
            ),
            "round {round}: {outcome:?}"
        );
        waits.push(waited);
        *outcomes.entry(format!("{outcome:?}")).or_insert(0_usize) += 1;
        wait_for_state(&hub, HANG_LIMIT, "the hang-up", |state| !state.connected);
        // Down then Up again moves the ladder through its grace straight to a new attempt.
        let inputs = running.inputs();
        inputs
            .blocking_send(Input::Device(DeviceEvent::Down(device())))
            .unwrap();
        device_up(inputs);
    }
    let (min, max) = range(&waits);
    report(&format!(
        "{ROUNDS} outcomes came after {min:?} to {max:?}: {outcomes:?}"
    ));
    let refused = outcomes
        .keys()
        .any(|outcome| outcome.starts_with("Refused"));
    assert!(
        outcomes.contains_key("Confirmed") && refused,
        "the race never went both ways: {outcomes:?}"
    );
    drop(running);
    pods.finish().unwrap();
}

#[test]
fn set_writes_in_the_turn_it_is_read() {
    let pods = Pods::new(Profile::default());
    let (running, _connects) = link_up(&pods);
    let hub = running.hub();
    let mut latencies = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let (mode, value) = noise_for(round);
        let frame = setting(NOISE, value);
        let written_before = round / 2;
        let started = Instant::now();
        let outcome = hub.submit(noise(mode));
        let heard = pods
            .wait_heard(&frame, written_before + 1, HANG_LIMIT)
            .unwrap();
        latencies.push(heard[written_before].at - started);
        // Confirmed only as the fake reports a setting back; assumed until measured on hardware.
        assert_eq!(block_on(outcome), Outcome::Confirmed, "round {round}");
    }
    let (min, max) = range(&latencies);
    report(&format!(
        "{ROUNDS} sets reached the pods {min:?} to {max:?} after submit"
    ));
    assert!(max < TURN_BOUND, "the slowest took {max:?}");
    drop(running);
    pods.finish().unwrap();
}

#[test]
fn oversized_sdu_is_dropped_and_the_link_stays_up() {
    let pods = Pods::new(Profile::default());
    let (running, _connects) = link_up(&pods);
    let oversized = vec![0x04; 5000];
    assert!(oversized.len() > RECV_BUFFER_LEN);
    pods.send(&oversized);
    let state = wait_for_battery(&pods, &running.hub(), SENTINEL);
    assert!(state.connected);
    drop(running);
    pods.finish().unwrap();
}

#[test]
fn noise_from_the_peer_never_stops_the_link() {
    let pods = Pods::new(Profile::default());
    let (running, _connects) = link_up(&pods);
    let mut source = Noise(0x2545_f491_4f6c_dd1d);
    for _ in 0..500 {
        // Never empty: a read of zero bytes is how a closed channel looks.
        let len = 1 + source.below(64);
        let sdu: Vec<u8> = (0..len).map(|_| source.next().to_le_bytes()[0]).collect();
        pods.send(&sdu);
    }
    let state = wait_for_battery(&pods, &running.hub(), SENTINEL);
    assert!(state.connected);
    drop(running);
    pods.finish().unwrap();
}

#[test]
fn stop_with_the_link_up_ends_the_reader_thread() {
    let pods = Pods::new(Profile::default());
    let (connector, _connects) = fake_connector(&pods);
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    let ended_after = runtime.block_on(async {
        let (link, inputs) = queue();
        let hub = Hub::new(link.clone(), State::default());
        let (requests, _unread) = mpsc::channel(REQUEST_QUEUE);
        let before = tasks();
        let driver = tokio::spawn(drive(
            hub.clone(),
            inputs,
            link.clone(),
            requests,
            connector,
            unread_outlets(),
            None,
        ));
        link.send(Input::Device(DeviceEvent::Up(device())))
            .await
            .unwrap();
        wait_for_state_async(&hub, HANG_LIMIT, "the first burst", filled).await;
        assert!(tasks() > before, "no reader thread while the link is up");
        stop(&link, driver).await;
        wait_for_tasks(before, Duration::from_secs(1)).await
    });
    report(&format!("the threads ended {ended_after:?} after the stop"));
    pods.finish().unwrap();
}

#[test]
fn stop_gives_up_on_a_driver_that_never_answers() {
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    let waited = runtime.block_on(async {
        let (link, inputs) = queue();
        let stuck = tokio::spawn(async move {
            let _held = inputs;
            std::future::pending::<()>().await;
        });
        let started = Instant::now();
        timeout(HANG_LIMIT, stop(&link, stuck)).await.unwrap();
        assert!(link.is_closed(), "the stuck driver was not aborted");
        started.elapsed()
    });
    let wait = Duration::from_millis(STOP_WAIT_MS);
    report(&format!("stop returned after {waited:?}"));
    assert!(
        waited >= wait && waited <= wait + REAL_TIME_SLACK,
        "{waited:?}, from {wait:?} to {:?}",
        wait + REAL_TIME_SLACK
    );
}

#[test]
fn never_more_than_one_reader_thread() {
    let live = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let (connected, connects) = std_mpsc::channel();
    let connector: Connector = {
        let (live, most) = (Arc::clone(&live), Arc::clone(&most));
        Arc::new(move |_| {
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            let name = thread::current().name().map(str::to_owned);
            connected.send((name, Instant::now())).ok();
            Err(ChannelError::Connect(Errno::HOSTDOWN))
        })
    };
    let ended = Arc::clone(&live);
    let tap = Box::new(move |input: &Input| {
        if let Input::Ended(_) = input {
            ended.fetch_sub(1, Ordering::SeqCst);
        }
    });
    let running = Running::start(connector, Some(tap));
    device_up(running.inputs());
    let mut times = Vec::with_capacity(LADDER_ATTEMPTS);
    for attempt in 1..=LADDER_ATTEMPTS {
        let (name, at) = connects
            .recv_timeout(HANG_LIMIT)
            .unwrap_or_else(|_| panic!("attempt {attempt} never connected"));
        assert_eq!(name.as_deref(), Some("link-reader"), "attempt {attempt}");
        times.push(at);
    }
    let gaps: Vec<Duration> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
    report(&format!(
        "the first {LADDER_ATTEMPTS} attempts came {gaps:?} apart"
    ));
    for (gap, delay) in gaps.iter().zip(HELD_DELAYS_MS) {
        let delay = Duration::from_millis(delay);
        assert!(
            *gap >= delay && *gap <= delay + REAL_TIME_SLACK,
            "{gap:?}, from {delay:?} to {:?}",
            delay + REAL_TIME_SLACK
        );
    }
    drop(running);
    assert_eq!(
        most.load(Ordering::SeqCst),
        1,
        "reader threads alive at once"
    );
}

#[test]
fn a_full_input_queue_refuses_with_timeout() {
    let (link, mut inputs) = queue();
    let hub = Hub::new(link, State::default());
    let ear = Command::SetEarDetection(pods_core::state::EarDetectionBehavior::Disabled);
    block_on(async {
        let waiting: Vec<_> = (0..INPUT_QUEUE).map(|_| hub.submit(ear)).collect();
        let refused = timeout(Duration::from_secs(1), hub.submit(ear)).await;
        assert_eq!(refused, Ok(Outcome::Refused(CommandError::Timeout)));
        for queued in 0..INPUT_QUEUE {
            let input = inputs.try_recv();
            assert!(
                matches!(input, Ok(Input::Command { .. })),
                "command {queued} was not queued"
            );
        }
        drop(inputs);
        for outcome in waiting {
            assert_eq!(outcome.await, Outcome::Refused(CommandError::Timeout));
        }
        let gone = timeout(Duration::from_secs(1), hub.submit(ear)).await;
        assert_eq!(gone, Ok(Outcome::Refused(CommandError::Timeout)));
    });
}

fn reader_inputs(connector: Connector, on_opened: impl Fn(&Channel)) -> Vec<String> {
    let (feedback, mut inputs) = mpsc::channel(INPUT_QUEUE);
    let reader = thread::spawn(move || reader::run(connector, device(), feedback));
    let labels = block_on(async {
        let mut labels = Vec::new();
        while let Some(input) = timeout(HANG_LIMIT, inputs.recv()).await.unwrap() {
            labels.push(match input {
                Input::Opened(channel) => {
                    on_opened(&channel);
                    "opened".to_owned()
                }
                Input::Sdu(sdu) => format!("sdu {}", sdu.len()),
                Input::Ended(End::Closed) => "ended closed".to_owned(),
                Input::Ended(End::ConnectFailed) => "ended connect failed".to_owned(),
                Input::Device(_)
                | Input::SendFailed
                | Input::Command { .. }
                | Input::Sleep(_)
                | Input::Shutdown(_)
                | Input::Audio(_)
                | Input::Media(_)
                | Input::Advert(_)
                | Input::Monitor(_) => {
                    format!("unexpected {input:?}")
                }
            });
        }
        labels
    });
    reader.join().unwrap();
    labels
}

#[test]
fn the_reader_sends_exactly_one_end() {
    let refused: Connector = Arc::new(|_| Err(ChannelError::Connect(Errno::HOSTDOWN)));
    assert_eq!(reader_inputs(refused, |_| {}), ["ended connect failed"]);

    let peer_closes: Connector = Arc::new(|_| {
        let (host_end, pods_end) = pair();
        pods_end.send(&[0x04, 0x00]).unwrap();
        Ok(host_end)
    });
    assert_eq!(
        reader_inputs(peer_closes, |_| {}),
        ["opened", "sdu 2", "ended closed"]
    );

    let (keep, kept) = std_mpsc::channel();
    let held_open: Connector = Arc::new(move |_| {
        let (host_end, pods_end) = pair();
        keep.send(pods_end).unwrap();
        Ok(host_end)
    });
    let shut = reader_inputs(held_open, |channel| channel.shutdown().unwrap());
    assert_eq!(shut, ["opened", "ended closed"]);
    drop(kept);
}
