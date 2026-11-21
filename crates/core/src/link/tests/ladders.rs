//! The recovery ladders, each rung at its exact time on the virtual clock.

use pods_proto::battery::{Batteries, Charge};
use pods_proto::model::Model;
use pods_proto::noise::NoiseMode;

use super::sim::{Answer, Input, Sim, ms};
use crate::link::Phase;
use crate::link::ladder::{Attempt, HELD_DELAYS_MS};

/// When each held attempt opens if every one fails at once.
const HELD_OPENS_MS: [u64; 11] = [
    0, 1000, 3000, 7000, 15000, 31000, 47000, 63000, 79000, 95000, 111_000,
];
/// The last held attempt fails here and the device parks.
pub(super) const PARKED_AT_MS: u64 = 111_000;
/// One hour of clock after the device parks.
const HOUR_MS: u64 = 3_600_000;
/// The wait between parked attempts, written out so a drifted ladder constant goes red.
pub(super) const PARKED_WAIT_MS: u64 = 300_000;
/// How long an absent device keeps its state.
const GRACE_MS: u64 = 7750;
/// How long an opened channel waits for each acknowledgement.
const ACK_WAIT_MS: u64 = 2000;
/// Most deadlines a refused device may pass before it parks, a loose bound so the test never reads the held table.
const DEADLINES_TO_PARK_MAX: usize = 64;

fn refused_until(until_ms: u64) -> Sim {
    let mut sim = Sim::answering(Answer::Refuse);
    sim.feed(Input::Up(0));
    sim.run_until(ms(until_ms));
    sim.check();
    sim
}

#[test]
#[expect(
    clippy::print_stdout,
    reason = "the printed attempt times are the record of this ladder"
)]
fn held_ladder_waits_match_the_table() {
    let sim = refused_until(PARKED_AT_MS);
    println!("held ladder opens at {:?} ms", sim.times_of("Open(0)"));
    assert_eq!(sim.times_of("Open(0)"), HELD_OPENS_MS);
    assert_eq!(
        sim.machine.phase(),
        Phase::Parked {
            until: ms(PARKED_AT_MS + PARKED_WAIT_MS)
        }
    );
    assert_eq!(sim.state.counters.reconnect_attempts, 10);
    assert_eq!(sim.state.counters.reconnect_failures, 1);
}

#[test]
#[expect(
    clippy::print_stdout,
    reason = "the printed attempt times are the record of this ladder"
)]
fn a_device_that_refuses_the_channel_is_parked_after_the_ladder() {
    let sim = refused_until(PARKED_AT_MS + HOUR_MS);
    let opens = sim.times_of("Open(0)");
    println!("parked opens at {:?} ms", &opens[HELD_OPENS_MS.len()..]);
    assert_eq!(&opens[..HELD_OPENS_MS.len()], HELD_OPENS_MS);
    let parked: Vec<u64> = (1..=HOUR_MS / PARKED_WAIT_MS)
        .map(|n| PARKED_AT_MS + n * PARKED_WAIT_MS)
        .collect();
    assert_eq!(&opens[HELD_OPENS_MS.len()..], parked);
    assert_eq!(sim.state.counters.reconnect_failures, 13);
    assert_eq!(sim.state.counters.reconnect_attempts, 22);
    assert!(matches!(sim.machine.phase(), Phase::Parked { .. }));
}

#[test]
fn a_channel_that_opens_and_never_acknowledges_counts_as_refused() {
    let mut sim = Sim::new();
    sim.feed(Input::Up(0));
    sim.run_until(ms(200_000));
    sim.check();
    let opens = sim.times_of("Open(0)");
    let closes = sim.times_of("Close");
    assert_eq!(opens.len(), HELD_OPENS_MS.len());
    assert_eq!(closes.len(), opens.len());
    for (open, close) in opens.iter().zip(&closes) {
        assert_eq!(close - open, ACK_WAIT_MS);
    }
    for ((close, next), delay) in closes.iter().zip(&opens[1..]).zip(HELD_DELAYS_MS) {
        assert_eq!(next - close, delay);
    }
    assert!(matches!(sim.machine.phase(), Phase::Parked { .. }));
    assert_eq!(sim.state.counters.reconnect_attempts, 10);
    assert_eq!(sim.state.counters.reconnect_failures, 1);
}

#[test]
fn a_parked_attempt_opens_on_the_parked_ladder() {
    let mut sim = refused_until(PARKED_AT_MS);
    sim.feed(Input::NextOpen(Answer::Hold));
    sim.run_until(ms(PARKED_AT_MS + PARKED_WAIT_MS));
    assert_eq!(
        sim.machine.phase(),
        Phase::Opening {
            attempt: Attempt::Parked
        }
    );
}

#[test]
fn a_parked_attempt_that_fails_parks_again() {
    let mut sim = Sim::answering(Answer::Refuse);
    sim.feed(Input::Up(0));
    for _ in 0..DEADLINES_TO_PARK_MAX {
        if matches!(sim.machine.phase(), Phase::Parked { .. }) {
            break;
        }
        sim.run_until(sim.machine.deadline().unwrap());
    }
    assert!(matches!(sim.machine.phase(), Phase::Parked { .. }));
    let until = sim.machine.deadline().unwrap();
    let opens = sim.times_of("Open(0)").len();
    let failures = sim.state.counters.reconnect_failures;
    sim.run_until(until);
    assert_eq!(sim.times_of("Open(0)").len(), opens + 1);
    assert_eq!(
        sim.machine.phase(),
        Phase::Parked {
            until: until.saturating_add(ms(PARKED_WAIT_MS))
        }
    );
    assert_eq!(sim.state.counters.reconnect_failures, failures + 1);
    sim.check();
}

#[test]
fn a_parked_device_re_arms_on_a_connected_edge() {
    let mut sim = refused_until(PARKED_AT_MS);
    sim.feed(Input::Down(0));
    assert_eq!(sim.machine.phase(), Phase::Idle);
    sim.feed(Input::NextOpen(Answer::Hold));
    sim.feed(Input::Up(0));
    assert_eq!(sim.last(), ["Open(0)"]);
    assert_eq!(
        sim.machine.phase(),
        Phase::Opening {
            attempt: Attempt::Held(1)
        }
    );
    sim.check();
}

#[test]
fn a_populated_device_parked_re_arms_through_the_grace() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    for _ in 0..HELD_OPENS_MS.len() {
        sim.feed(Input::NextOpen(Answer::Refuse));
    }
    sim.feed(Input::PeerCloses);
    sim.run_until(ms(200_000));
    assert!(matches!(sim.machine.phase(), Phase::Parked { .. }));
    sim.feed(Input::Down(0));
    assert_eq!(
        sim.machine.phase(),
        Phase::Grace {
            until: ms(200_000 + GRACE_MS)
        }
    );
    sim.feed(Input::NextOpen(Answer::Hold));
    sim.feed(Input::Up(0));
    assert_eq!(sim.last(), ["Open(0)"]);
    assert_eq!(
        sim.machine.phase(),
        Phase::Opening {
            attempt: Attempt::Held(1)
        }
    );
    sim.check();
}

#[test]
fn loss_of_an_up_channel_retries_after_750_ms() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.run_until(ms(10_000));
    sim.feed(Input::NextOpen(Answer::Hold));
    sim.feed(Input::PeerCloses);
    assert_eq!(
        sim.machine.phase(),
        Phase::Backoff {
            attempt: Attempt::Held(1),
            until: ms(10_750)
        }
    );
    assert!(!sim.state.connected);
    sim.run_until(ms(10_749));
    assert_eq!(sim.times_of("Open(0)"), [0]);
    sim.run_until(ms(10_750));
    assert_eq!(sim.times_of("Open(0)"), [0, 10_750]);
    assert_eq!(
        sim.machine.phase(),
        Phase::Opening {
            attempt: Attempt::Held(1)
        }
    );
    assert_eq!(sim.state.counters.reconnect_attempts, 1);
    sim.feed(Input::Release(true));
    sim.feed(Input::Packet(pods_samples::handshake_ack()));
    sim.feed(Input::Packet(pods_samples::features_ack()));
    assert!(matches!(sim.machine.phase(), Phase::Up { .. }));
    assert_eq!(sim.times_of("Send(handshake)"), [0, 10_750]);
    sim.check();
}

#[test]
fn a_failed_retry_after_a_loss_climbs_the_held_ladder() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.feed(Input::NextOpen(Answer::Refuse));
    sim.feed(Input::NextOpen(Answer::Hold));
    sim.feed(Input::PeerCloses);
    sim.run_until(ms(2000));
    assert_eq!(sim.times_of("Open(0)"), [0, 750, 1750]);
    assert_eq!(
        sim.machine.phase(),
        Phase::Opening {
            attempt: Attempt::Held(2)
        }
    );
}

fn populated_sim() -> Sim {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.state.device_name = "Test AirPods".into();
    sim.state.model_number = "A3063".into();
    sim.state.model = Model::from_number("A3063");
    sim.state.noise_mode = NoiseMode::Anc;
    sim.state.batteries.left = Some(Charge {
        level: 80,
        charging: false,
    });
    sim
}

#[test]
fn absent_grace_resets_the_device_after_7750_ms() {
    let mut sim = populated_sim();
    sim.run_until(ms(5000));
    sim.feed(Input::Down(0));
    assert_eq!(sim.step_of("Down(0)").actions, ["Close"]);
    assert_eq!(sim.machine.phase(), Phase::Grace { until: ms(12_750) });
    sim.run_until(ms(12_749));
    assert_eq!(sim.state.device_name, "Test AirPods");
    sim.run_until(ms(12_750));
    assert_eq!(sim.last(), ["Sweep"]);
    assert_eq!(sim.machine.phase(), Phase::Idle);
    let mut reset = sim.state.clone();
    reset.reset_device();
    assert_eq!(sim.state, reset);
    assert_eq!(sim.state.device_name, "");
    assert_eq!(sim.state.model_number, "A3063");
    assert_eq!(sim.state.batteries, Batteries::default());
    assert_eq!(sim.state.counters.reconnect_failures, 1);
    assert!(sim.machine.active.is_none());
    sim.feed(Input::NextOpen(Answer::Refuse));
    sim.feed(Input::Up(0));
    sim.feed(Input::Down(0));
    assert_eq!(sim.machine.phase(), Phase::Idle);
    sim.check();
}

#[test]
fn device_back_within_the_grace_keeps_its_state() {
    let mut sim = populated_sim();
    sim.feed(Input::Down(0));
    let kept = sim.state.clone();
    sim.run_until(ms(7749));
    sim.feed(Input::NextOpen(Answer::Hold));
    sim.feed(Input::Up(0));
    assert_eq!(sim.last(), ["Open(0)"]);
    assert_eq!(
        sim.machine.phase(),
        Phase::Opening {
            attempt: Attempt::Held(1)
        }
    );
    assert_eq!(sim.state, kept);
    sim.run_until(ms(20_000));
    assert_eq!(sim.state, kept);
    sim.check();
}

/// Feeds a tick 1 ms before the deadline and asserts nothing moves.
fn early_tick_does_nothing(sim: &mut Sim) {
    let phase = sim.machine.phase();
    let deadline = sim.machine.deadline().unwrap();
    sim.run_until(deadline.checked_sub(ms(1)).unwrap());
    sim.feed(Input::Tick);
    assert_eq!(sim.last(), Vec::<String>::new());
    assert_eq!(sim.machine.phase(), phase);
}

#[test]
fn a_tick_before_a_deadline_does_nothing() {
    let mut sim = Sim::new();
    sim.feed(Input::Up(0));
    early_tick_does_nothing(&mut sim);
    sim.feed(Input::Packet(pods_samples::handshake_ack()));
    early_tick_does_nothing(&mut sim);
    sim.feed(Input::Packet(pods_samples::features_ack()));
    early_tick_does_nothing(&mut sim);
    sim.feed(Input::NextOpen(Answer::Refuse));
    sim.feed(Input::PeerCloses);
    early_tick_does_nothing(&mut sim);
    sim.run_until(sim.machine.deadline().unwrap());
    assert!(matches!(
        sim.machine.phase(),
        Phase::Backoff {
            attempt: Attempt::Held(2),
            ..
        }
    ));
    early_tick_does_nothing(&mut sim);
    sim.feed(Input::Down(0));
    assert!(matches!(sim.machine.phase(), Phase::Grace { .. }));
    early_tick_does_nothing(&mut sim);
    sim.check();
}
