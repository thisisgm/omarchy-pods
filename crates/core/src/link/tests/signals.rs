//! The signals: one `Up` on entering `Up`, one `Lost` at each device reset, saying when the link last left `Up`, if it was up.

use std::time::Duration;

use super::sim::{Answer, Input, Sim, addr, ms};
use crate::command::Command;
use crate::link::Signal;
use crate::link::ladder::{ABSENT_GRACE_MS, FIRST_RETRY_MS, NOTIFY_REREQUEST_MS};

fn up() -> Signal {
    Signal::Up { device: addr(0) }
}

fn lost(down_at: Option<Duration>) -> Signal {
    Signal::Lost {
        device: addr(0),
        down_at,
    }
}

/// The signals since `from`, without their times.
fn signals_since(sim: &Sim, from: usize) -> Vec<Signal> {
    sim.signals
        .iter()
        .skip(from)
        .map(|(_, signal)| signal.clone())
        .collect()
}

#[test]
fn entering_up_signals_once_and_the_repeated_request_does_not() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    assert_eq!(sim.signals, [(sim.now, up())]);
    sim.feed(Input::Wait(ms(NOTIFY_REREQUEST_MS)));
    assert_eq!(sim.last(), ["Send(notifications)"]);
    assert_eq!(signals_since(&sim, 1), []);
    sim.check();
}

#[test]
fn pods_gone_for_good_signal_lost_when_the_grace_ends() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.feed(Input::Down(0));
    let down = sim.now;
    sim.feed(Input::Wait(ms(ABSENT_GRACE_MS).saturating_sub(ms(1))));
    assert_eq!(signals_since(&sim, 1), []);
    sim.feed(Input::Wait(ms(1)));
    let reset = down.saturating_add(ms(ABSENT_GRACE_MS));
    assert_eq!(sim.signals[1..], [(reset, lost(Some(down)))]);
    sim.check();
}

#[test]
fn a_device_that_never_came_up_signals_lost_unpopulated_when_it_leaves() {
    let mut sim = Sim::answering(Answer::Refuse);
    sim.feed(Input::Up(0));
    assert_eq!(signals_since(&sim, 0), []);
    sim.feed(Input::Down(0));
    assert_eq!(signals_since(&sim, 0), [lost(None)]);
    sim.check();
}

#[test]
fn a_user_disconnect_signals_lost_at_once() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.feed(Input::Command(Command::Disconnect));
    sim.feed(Input::Down(0));
    let reset = sim.now;
    sim.feed(Input::Wait(ms(ABSENT_GRACE_MS)));
    assert_eq!(sim.signals[1..], [(reset, lost(Some(reset)))]);
    sim.check();
}

#[test]
fn a_device_removed_while_up_signals_lost_once() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    // Up for 1 ms past a repeated request, so the stamp is the leaving step's, not the entry's or the request's.
    sim.feed(Input::Wait(ms(NOTIFY_REREQUEST_MS + 1)));
    let gone = sim.now;
    sim.feed(Input::Gone(0));
    sim.feed(Input::Wait(Duration::from_secs(1)));
    assert_eq!(signals_since(&sim, 1), [lost(Some(gone))]);
    sim.check();
}

#[test]
fn a_device_removed_while_waiting_to_retry_signals_lost() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    let closed = sim.now;
    sim.feed(Input::PeerCloses);
    assert_eq!(signals_since(&sim, 1), []);
    sim.feed(Input::Wait(ms(FIRST_RETRY_MS - 1)));
    sim.feed(Input::Gone(0));
    assert_eq!(signals_since(&sim, 1), [lost(Some(closed))]);
    sim.check();
}

#[test]
fn a_shutdown_signals_nothing() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.feed(Input::Shutdown);
    assert_eq!(signals_since(&sim, 1), []);
    sim.check();
}
