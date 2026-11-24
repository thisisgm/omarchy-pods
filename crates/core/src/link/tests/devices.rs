//! Support tests: which device `connect`, `disconnect` and `forget` act on, and what each asks of BlueZ.

use super::ladders::PARKED_AT_MS;
use super::sim::{Answer, Input, NO_ACTIONS, Sim, millis, ms};
use crate::command::{Command, CommandError, Outcome};
use crate::link::ladder::{Attempt, HELD_DELAYS_MS};
use crate::link::{DeviceAction, Phase};
use crate::state::Counters;

/// How long an absent device keeps its state.
const GRACE_MS: u64 = 7750;

/// The wait before the first dial after a link that was up ends.
const FIRST_RETRY_MS: u64 = 750;

/// A link up to device 0 with its name reported.
fn up_named() -> Sim {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.feed(Input::Packet(pods_samples::metadata()));
    assert_ne!(sim.state.device_name, "");
    sim
}

#[test]
fn connect_while_up_is_confirmed_without_a_call() {
    let mut sim = up_named();
    let counters = sim.state.counters;
    sim.feed(Input::Command(Command::Connect));
    assert_eq!(sim.last(), ["Resolve(0)"]);
    assert_eq!(sim.outcomes(), [Outcome::Confirmed]);
    assert_eq!(sim.state.counters, counters);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn connect_on_a_parked_device_restarts_the_ladder() {
    let mut sim = Sim::answering(Answer::Refuse);
    sim.feed(Input::Up(0));
    sim.run_until(ms(PARKED_AT_MS));
    assert!(matches!(sim.machine.phase(), Phase::Parked { .. }));
    let counters = sim.state.counters;
    sim.feed(Input::NextOpen(Answer::Hold));
    sim.feed(Input::Command(Command::Connect));
    assert_eq!(sim.last(), ["Open(0)", "Resolve(0)"]);
    assert_eq!(sim.outcomes(), [Outcome::Sent]);
    assert_eq!(
        sim.machine.phase(),
        Phase::Opening {
            attempt: Attempt::Held(1)
        }
    );
    assert_eq!(sim.state.counters, counters);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn connect_with_no_link_calls_bluez_and_is_sent() {
    let mut sim = Sim::new();
    sim.feed(Input::Down(0));
    sim.feed(Input::Command(Command::Connect));
    assert_eq!(sim.last(), ["Call(0, Connect)", "Resolve(0)"]);
    assert_eq!(sim.outcomes(), [Outcome::Sent]);
    assert_eq!(sim.state.counters.connect_calls, 1);
    assert_eq!(sim.machine.phase(), Phase::Idle);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn connect_with_nothing_known_is_no_device() {
    let mut sim = Sim::new();
    for command in [Command::Connect, Command::Disconnect, Command::Forget] {
        sim.feed(Input::Command(command));
        assert_eq!(sim.last().len(), 1, "{command:?}");
    }
    assert_eq!(
        sim.outcomes(),
        [Outcome::Refused(CommandError::NoDevice); 3]
    );
    assert_eq!(sim.state.counters, Counters::default());
    sim.check_all_tickets_resolved_once();
}

#[test]
fn connect_with_two_known_and_no_history_is_no_device() {
    let mut sim = Sim::new();
    sim.feed(Input::Down(0));
    sim.feed(Input::Down(1));
    sim.feed(Input::Command(Command::Connect));
    assert_eq!(sim.last(), ["Resolve(0)"]);
    assert_eq!(sim.outcomes(), [Outcome::Refused(CommandError::NoDevice)]);
    assert_eq!(sim.state.counters.connect_calls, 0);
    sim.check_all_tickets_resolved_once();
}

/// Device 1 linked last and gone idle, after device 0 was known first.
fn second_linked_then_idle() -> Sim {
    let mut sim = Sim::new();
    sim.feed(Input::Down(0));
    sim.bring_up(1);
    sim.feed(Input::Down(1));
    sim.feed(Input::Wait(ms(GRACE_MS)));
    assert_eq!(sim.machine.phase(), Phase::Idle);
    sim
}

#[test]
fn connect_prefers_the_last_linked_device() {
    let mut sim = second_linked_then_idle();
    sim.feed(Input::Command(Command::Connect));
    assert_eq!(sim.last(), ["Call(1, Connect)", "Resolve(0)"]);
    assert_eq!(sim.outcomes(), [Outcome::Sent]);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn connect_prefers_the_active_device_over_the_last_linked() {
    let mut sim = second_linked_then_idle();
    sim.feed(Input::NextOpen(Answer::Hold));
    sim.feed(Input::Up(0));
    assert!(matches!(sim.machine.phase(), Phase::Opening { .. }));
    sim.feed(Input::Command(Command::Connect));
    assert_eq!(sim.last(), ["Call(0, Connect)", "Resolve(0)"]);
    assert_eq!(sim.outcomes(), [Outcome::Sent]);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn disconnect_calls_bluez_and_skips_the_grace() {
    let mut sim = up_named();
    sim.feed(Input::Command(Command::Disconnect));
    assert_eq!(sim.last(), ["Call(0, Disconnect)", "Resolve(0)"]);
    assert_eq!(sim.outcomes(), [Outcome::Sent]);
    assert_eq!(sim.state.counters.disconnect_calls, 1);
    sim.feed(Input::Down(0));
    assert_eq!(sim.step_of("Down(0)").actions, ["Close"]);
    let end = sim.step_of("Ended(Closed)");
    assert_eq!(end.actions, ["Sweep"]);
    assert_eq!(end.at, sim.step_of("Down(0)").at);
    assert_eq!(sim.machine.phase(), Phase::Idle);
    assert_eq!(sim.state.device_name, "");
    assert_eq!(sim.state.counters.reconnect_failures, 0);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn disconnect_when_not_connected_is_refused() {
    let mut grace = up_named();
    grace.feed(Input::Down(0));
    assert!(matches!(grace.machine.phase(), Phase::Grace { .. }));
    let mut idle = Sim::new();
    idle.feed(Input::Down(0));
    for (name, mut sim) in [("grace", grace), ("idle", idle)] {
        let counters = sim.state.counters;
        sim.feed(Input::Command(Command::Disconnect));
        assert_eq!(sim.last(), ["Resolve(0)"], "{name}");
        assert_eq!(
            sim.outcomes(),
            [Outcome::Refused(CommandError::NotConnected)],
            "{name}"
        );
        assert_eq!(sim.state.counters, counters, "{name}");
        sim.check_all_tickets_resolved_once();
    }
}

#[test]
fn forget_calls_bluez() {
    let mut sim = up_named();
    sim.feed(Input::Command(Command::Forget));
    assert_eq!(sim.last(), ["Call(0, Forget)", "Resolve(0)"]);
    assert_eq!(sim.outcomes(), [Outcome::Sent]);
    assert_eq!(sim.state.counters.forget_calls, 1);
    assert!(matches!(sim.machine.phase(), Phase::Up { .. }));
    sim.feed(Input::Gone(0));
    assert_eq!(sim.state.device_name, "");
    sim.feed(Input::Command(Command::Connect));
    assert_eq!(sim.outcome(1), [Outcome::Refused(CommandError::NoDevice)]);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_failed_call_raises_its_counter_only() {
    for action in [
        DeviceAction::Connect,
        DeviceAction::Disconnect,
        DeviceAction::Forget,
    ] {
        let mut sim = up_named();
        let phase = sim.machine.phase();
        let mut expected = sim.state.clone();
        match action {
            DeviceAction::Connect => expected.counters.connect_failures = 1,
            DeviceAction::Disconnect => expected.counters.disconnect_failures = 1,
            DeviceAction::Forget => {}
        }
        sim.feed(Input::CallFailed(action));
        assert_eq!(sim.last(), NO_ACTIONS, "{action:?}");
        assert_eq!(sim.machine.phase(), phase, "{action:?}");
        assert_eq!(sim.state, expected, "{action:?}");
        sim.check();
    }
    let mut sim = up_named();
    sim.feed(Input::Command(Command::Disconnect));
    sim.feed(Input::CallFailed(DeviceAction::Disconnect));
    sim.feed(Input::Down(0));
    assert!(matches!(sim.machine.phase(), Phase::Grace { .. }));
    assert_ne!(sim.state.device_name, "");
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_second_disconnect_skips_the_grace_when_the_first_fails() {
    let mut sim = up_named();
    sim.feed(Input::Command(Command::Disconnect));
    sim.feed(Input::Command(Command::Disconnect));
    assert_eq!(sim.state.counters.disconnect_calls, 2);
    sim.feed(Input::CallFailed(DeviceAction::Disconnect));
    sim.feed(Input::Down(0));
    assert_eq!(sim.machine.phase(), Phase::Idle);
    assert_eq!(sim.state.device_name, "");
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_disconnect_that_fails_after_the_pods_came_back_leaves_a_newer_one_holding() {
    let mut sim = up_named();
    sim.feed(Input::Command(Command::Disconnect));
    sim.feed(Input::Down(0));
    assert_eq!(sim.machine.phase(), Phase::Idle);
    sim.bring_up(0);
    sim.feed(Input::Packet(pods_samples::metadata()));
    sim.feed(Input::Command(Command::Disconnect));
    sim.feed(Input::FirstCallFailed(DeviceAction::Disconnect));
    assert_eq!(sim.state.counters.disconnect_failures, 1);
    sim.feed(Input::Down(0));
    assert_eq!(sim.machine.phase(), Phase::Idle);
    assert_eq!(sim.state.device_name, "");
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_disconnect_that_ends_the_channel_first_dials_nothing_until_bluez_says_it_left() {
    let mut sim = up_named();
    sim.feed(Input::Command(Command::Disconnect));
    sim.feed(Input::PeerCloses);
    sim.feed(Input::Wait(ms(GRACE_MS)));
    assert_eq!(sim.times_of("Open(0)").len(), 1);
    sim.feed(Input::Down(0));
    assert_eq!(sim.machine.phase(), Phase::Idle);
    assert_eq!(sim.state.device_name, "");
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_disconnect_that_fails_after_holding_the_dial_lets_the_ladder_dial_again() {
    let mut sim = up_named();
    sim.feed(Input::Command(Command::Disconnect));
    sim.feed(Input::PeerCloses);
    sim.feed(Input::Wait(ms(FIRST_RETRY_MS)));
    assert_eq!(sim.times_of("Open(0)").len(), 1);
    let failed_at = millis(sim.now);
    sim.feed(Input::CallFailed(DeviceAction::Disconnect));
    sim.feed(Input::Wait(ms(FIRST_RETRY_MS)));
    assert_eq!(sim.times_of("Open(0)")[1..], [failed_at + FIRST_RETRY_MS]);
    sim.check();
}

#[test]
fn a_disconnect_that_reset_the_pods_does_not_stop_the_dial_after_their_next_close() {
    let mut sim = up_named();
    sim.feed(Input::Command(Command::Disconnect));
    sim.feed(Input::Down(0));
    assert_eq!(sim.machine.phase(), Phase::Idle);
    sim.bring_up(0);
    let closed_at = millis(sim.now);
    sim.feed(Input::PeerCloses);
    sim.feed(Input::Wait(ms(FIRST_RETRY_MS)));
    assert_eq!(sim.times_of("Open(0)")[2..], [closed_at + FIRST_RETRY_MS]);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_disconnect_the_pods_left_and_came_back_from_does_not_stop_the_next_dial() {
    let mut sim = Sim::new();
    sim.feed(Input::NextOpen(Answer::Hold));
    sim.feed(Input::Up(0));
    sim.feed(Input::Command(Command::Disconnect));
    sim.feed(Input::Down(0));
    sim.feed(Input::Up(0));
    sim.feed(Input::Release(false));
    sim.feed(Input::Wait(ms(HELD_DELAYS_MS[0])));
    assert_eq!(sim.times_of("Open(0)"), [0, HELD_DELAYS_MS[0]]);
    sim.check_all_tickets_resolved_once();
}

/// A link up again through a connect taken after the disconnect ended the channel and before BlueZ said the pods left.
fn up_again_after_a_connect_ahead_of_the_disconnects_device_down() -> Sim {
    let mut sim = up_named();
    sim.feed(Input::Command(Command::Disconnect));
    sim.feed(Input::PeerCloses);
    sim.feed(Input::NextOpen(Answer::Hold));
    sim.feed(Input::Command(Command::Connect));
    assert_eq!(sim.last(), ["Open(0)", "Resolve(1)"]);
    sim.feed(Input::Down(0));
    sim.feed(Input::Up(0));
    sim.feed(Input::Release(true));
    sim.feed(Input::Packet(pods_samples::handshake_ack()));
    sim.feed(Input::Packet(pods_samples::features_ack()));
    assert!(matches!(sim.machine.phase(), Phase::Up { .. }));
    sim
}

#[test]
fn a_connect_taken_before_bluez_says_the_disconnected_pods_left_keeps_the_dial_after_a_close() {
    let mut sim = up_again_after_a_connect_ahead_of_the_disconnects_device_down();
    let closed_at = millis(sim.now);
    sim.feed(Input::PeerCloses);
    sim.feed(Input::Wait(ms(FIRST_RETRY_MS)));
    assert_eq!(sim.times_of("Open(0)")[2..], [closed_at + FIRST_RETRY_MS]);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_connect_taken_before_bluez_says_the_disconnected_pods_left_keeps_the_grace_of_a_dropout() {
    let mut sim = up_again_after_a_connect_ahead_of_the_disconnects_device_down();
    sim.feed(Input::Down(0));
    assert!(matches!(sim.machine.phase(), Phase::Grace { .. }));
    assert_ne!(sim.state.device_name, "");
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_disconnect_that_fails_after_the_pods_left_and_came_back_leaves_a_newer_one_holding() {
    let mut sim = up_again_after_a_connect_ahead_of_the_disconnects_device_down();
    sim.feed(Input::Command(Command::Disconnect));
    sim.feed(Input::FirstCallFailed(DeviceAction::Disconnect));
    assert_eq!(sim.state.counters.disconnect_failures, 1);
    sim.feed(Input::PeerCloses);
    sim.feed(Input::Wait(ms(GRACE_MS)));
    assert_eq!(sim.times_of("Open(0)").len(), 2);
    sim.feed(Input::Down(0));
    assert_eq!(sim.machine.phase(), Phase::Idle);
    assert_eq!(sim.state.device_name, "");
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_disconnect_still_holds_the_dial_when_bluez_repeats_that_the_pods_are_connected() {
    let mut sim = up_named();
    sim.feed(Input::Command(Command::Disconnect));
    sim.feed(Input::Up(0));
    sim.feed(Input::PeerCloses);
    sim.feed(Input::Wait(ms(GRACE_MS)));
    assert_eq!(sim.times_of("Open(0)").len(), 1);
    sim.feed(Input::Down(0));
    assert_eq!(sim.machine.phase(), Phase::Idle);
    assert_eq!(sim.state.device_name, "");
    sim.check_all_tickets_resolved_once();
}
