//! Support tests: `Resumed` in every phase: a waiting ladder dials now and goes on from its attempt, a grace starts again, a stopped machine does nothing, and every other phase only sweeps.

use super::ladders::PARKED_AT_MS;
use super::sim::{Answer, Input, NO_ACTIONS, Sim, ms};
use crate::link::Phase;
use crate::link::ladder::{ABSENT_GRACE_MS, Attempt, HELD_DELAYS_MS, PARKED_RETRY_MS};

/// Where the clock stands when the wake comes, inside every wait below.
const WOKE_AFTER_MS: u64 = 500;

/// Feeds `Resumed` and gives the actions of its own step.
fn resume(sim: &mut Sim) -> Vec<String> {
    sim.feed(Input::Resumed);
    sim.step_of("Resumed").actions.clone()
}

/// Feeds `Resumed` to a machine in a phase that only sweeps, and checks the phase and the state stay as they were.
fn only_sweeps(mut sim: Sim) {
    let phase = sim.machine.phase();
    let state = sim.state.clone();
    assert_eq!(resume(&mut sim), ["Sweep"], "in {phase:?}");
    assert_eq!(sim.machine.phase(), phase);
    assert_eq!(sim.state, state);
    sim.check();
}

#[test]
fn idle_only_sweeps() {
    only_sweeps(Sim::new());
}

#[test]
fn opening_only_sweeps() {
    let mut sim = Sim::answering(Answer::Hold);
    sim.feed(Input::Up(0));
    assert!(matches!(sim.machine.phase(), Phase::Opening { .. }));
    only_sweeps(sim);
}

#[test]
fn either_handshake_step_only_sweeps() {
    let mut sim = Sim::new();
    sim.feed(Input::Up(0));
    assert!(matches!(
        sim.machine.phase(),
        Phase::AwaitHandshakeAck { .. }
    ));
    only_sweeps(sim);
    let mut sim = Sim::new();
    sim.feed(Input::Up(0));
    sim.feed(Input::Packet(pods_samples::handshake_ack()));
    assert!(matches!(
        sim.machine.phase(),
        Phase::AwaitFeaturesAck { .. }
    ));
    only_sweeps(sim);
}

#[test]
fn up_only_sweeps() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    only_sweeps(sim);
}

#[test]
fn closing_only_sweeps() {
    let mut sim = Sim::answering(Answer::Hold);
    sim.feed(Input::Up(0));
    sim.feed(Input::Gone(0));
    assert_eq!(sim.machine.phase(), Phase::Closing { attempt: None });
    only_sweeps(sim);
}

#[test]
fn backoff_dials_now_and_goes_on_from_its_attempt() {
    let mut sim = Sim::answering(Answer::Refuse);
    sim.feed(Input::Up(0));
    sim.feed(Input::Wait(ms(WOKE_AFTER_MS)));
    assert_eq!(
        sim.machine.phase(),
        Phase::Backoff {
            attempt: Attempt::Held(2),
            until: ms(HELD_DELAYS_MS[0])
        }
    );
    assert_eq!(resume(&mut sim), ["Open(0)", "Sweep"]);
    assert_eq!(
        sim.machine.phase(),
        Phase::Backoff {
            attempt: Attempt::Held(3),
            until: ms(WOKE_AFTER_MS + HELD_DELAYS_MS[1])
        }
    );
    assert_eq!(sim.state.counters.reconnect_attempts, 1);
    sim.check();
}

#[test]
fn parked_dials_now_and_parks_again_from_the_wake() {
    let mut sim = Sim::answering(Answer::Refuse);
    sim.feed(Input::Up(0));
    sim.run_until(ms(PARKED_AT_MS + WOKE_AFTER_MS));
    assert_eq!(
        sim.machine.phase(),
        Phase::Parked {
            until: ms(PARKED_AT_MS + PARKED_RETRY_MS)
        }
    );
    assert_eq!(resume(&mut sim), ["Open(0)", "Sweep"]);
    assert_eq!(
        sim.machine.phase(),
        Phase::Parked {
            until: ms(PARKED_AT_MS + WOKE_AFTER_MS + PARKED_RETRY_MS)
        }
    );
    assert_eq!(sim.state.counters.reconnect_failures, 2);
    sim.check();
}

#[test]
fn grace_starts_again_from_the_wake() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    let down = sim.now;
    sim.feed(Input::Down(0));
    sim.feed(Input::Wait(ms(WOKE_AFTER_MS)));
    assert_eq!(
        sim.machine.phase(),
        Phase::Grace {
            until: down.saturating_add(ms(ABSENT_GRACE_MS))
        }
    );
    let state = sim.state.clone();
    assert_eq!(resume(&mut sim), ["Sweep"]);
    assert_eq!(
        sim.machine.phase(),
        Phase::Grace {
            until: sim.now.saturating_add(ms(ABSENT_GRACE_MS))
        }
    );
    assert_eq!(sim.state, state);
    sim.check();
}

#[test]
fn stopped_does_nothing_at_all() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.feed(Input::Shutdown);
    assert_eq!(resume(&mut sim), NO_ACTIONS);
    assert_eq!(sim.machine.phase(), Phase::Stopped);
    sim.check();
}
