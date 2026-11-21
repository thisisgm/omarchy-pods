//! The machine under arbitrary input: whatever arrives, the invariants hold and every ticket resolves once.

use proptest::prelude::*;

use super::sim::{Answer, Input, Sim, ms};
use crate::command::tests::any_command;

/// The longest random packet, one receive buffer.
const PACKET_MAX: usize = 4096;
/// The longest clock step, past one parked interval.
const WAIT_MAX_MS: u64 = 400_000;
/// The longest step that stays inside a handshake step or a grace, so the link also gets up.
const SHORT_WAIT_MAX_MS: u64 = 3000;

fn device() -> impl Strategy<Value = usize> {
    0_usize..2
}

fn packet() -> impl Strategy<Value = Vec<u8>> {
    let mut samples = pods_samples::first_burst();
    samples.push(pods_samples::control());
    let acks = vec![pods_samples::handshake_ack(), pods_samples::features_ack()];
    prop_oneof![
        4 => prop::sample::select(acks),
        2 => prop::sample::select(samples),
        1 => prop::collection::vec(any::<u8>(), 0..=PACKET_MAX),
    ]
}

fn answer() -> impl Strategy<Value = Answer> {
    prop_oneof![Just(Answer::Open), Just(Answer::Refuse), Just(Answer::Hold)]
}

fn input() -> impl Strategy<Value = Input> {
    prop_oneof![
        3 => device().prop_map(Input::Up),
        2 => device().prop_map(Input::Down),
        1 => device().prop_map(Input::Gone),
        1 => Just(Input::BluezLost),
        6 => packet().prop_map(Input::Packet),
        2 => any_command().prop_map(Input::Command),
        1 => Just(Input::CallFailed),
        2 => Just(Input::Tick),
        3 => (0..=SHORT_WAIT_MAX_MS).prop_map(|wait| Input::Wait(ms(wait))),
        1 => (0..=WAIT_MAX_MS).prop_map(|wait| Input::Wait(ms(wait))),
        2 => answer().prop_map(Input::NextOpen),
        2 => any::<bool>().prop_map(Input::Release),
        1 => Just(Input::PeerCloses),
    ]
}

fn events(len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<Input>> {
    prop::collection::vec(input(), len)
}

proptest! {
    #[test]
    fn any_event_sequence_keeps_the_invariants(script in events(0..200)) {
        let mut sim = Sim::new();
        for item in script {
            sim.feed(item);
            sim.check();
        }
        sim.feed(Input::Shutdown);
        sim.check_all_tickets_resolved_once();
    }
}
