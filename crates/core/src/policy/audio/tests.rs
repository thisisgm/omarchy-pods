//! Support tests: the follow request each link signal makes, and what `audio_server` reads after each fact.

use std::time::Duration;

use pods_proto::addr::BdAddr;
use pods_proto::uplink::UplinkState;

use super::{
    AudioFact, AudioRequest, Percent, SinkFact, SinkState, card_name, server_after, sink_prefix,
};
use crate::link::Signal;
use crate::policy::{Effect, Fact, Policy, SleepEdge};
use crate::state::{AudioServer, State};

/// A synthetic address, never a real one.
const DEVICE: &str = "02:00:00:00:00:01";

fn device() -> BdAddr {
    DEVICE.parse().unwrap()
}

fn up() -> Fact {
    Fact::Link(Signal::Up { device: device() })
}

fn lost() -> Fact {
    Fact::Link(Signal::Lost {
        device: device(),
        down_at: None,
    })
}

fn sink() -> Fact {
    let volume = Percent::new(40).unwrap();
    let is_default = true;
    let state = SinkState::Running;
    Fact::Audio(AudioFact::Sink(Some(SinkFact {
        is_default,
        volume,
        state,
    })))
}

const SERVERS: [AudioServer; 3] = [AudioServer::Idle, AudioServer::Up, AudioServer::Down];

#[test]
fn the_link_up_follows_its_device_and_the_loss_follows_none() {
    let mut policy = Policy::default();
    let state = State::default();
    let follow = |device| Effect::Audio(AudioRequest::Follow(device));
    assert_eq!(
        policy.step(&state, up(), Duration::ZERO),
        [follow(Some(device()))]
    );
    assert_eq!(policy.step(&state, lost(), Duration::ZERO), [follow(None)]);
    assert_eq!(policy.step(&state, sink(), Duration::ZERO), []);
    assert_eq!(
        policy.step(&state, Fact::Audio(AudioFact::Up), Duration::ZERO),
        []
    );
}

#[test]
fn the_link_sets_down_then_idle_whatever_was_read() {
    for was in SERVERS {
        assert_eq!(server_after(was, &up()), AudioServer::Down, "{was:?}");
        assert_eq!(server_after(was, &lost()), AudioServer::Idle, "{was:?}");
    }
}

#[test]
fn the_adapter_is_heard_only_while_a_device_is_followed() {
    let reported = [
        (AudioFact::Up, AudioServer::Up),
        (AudioFact::Down, AudioServer::Down),
    ];
    for (fact, reads) in reported {
        assert_eq!(server_after(AudioServer::Down, &Fact::Audio(fact)), reads);
        assert_eq!(server_after(AudioServer::Up, &Fact::Audio(fact)), reads);
        let late = server_after(AudioServer::Idle, &Fact::Audio(fact));
        assert_eq!(late, AudioServer::Idle, "{fact:?}");
    }
}

#[test]
fn a_sink_or_a_state_change_leaves_the_reading() {
    for was in SERVERS {
        assert_eq!(server_after(was, &sink()), was, "{was:?}");
        assert_eq!(server_after(was, &Fact::Changed), was, "{was:?}");
    }
}

#[test]
fn an_uplink_marker_asks_nothing_and_leaves_the_reading() {
    for said in [UplinkState::Running, UplinkState::Stopped] {
        let marker = || Fact::Link(Signal::Uplink(said));
        let effects = Policy::default().step(&State::default(), marker(), Duration::ZERO);
        let asked = effects
            .iter()
            .any(|effect| matches!(effect, Effect::Audio(_)));
        assert!(!asked, "{said:?}: {effects:?}");
        for was in SERVERS {
            assert_eq!(server_after(was, &marker()), was, "{said:?} {was:?}");
        }
    }
}

#[test]
fn a_suspend_or_a_tick_leaves_the_reading() {
    for was in SERVERS {
        for edge in [SleepEdge::Sleeping, SleepEdge::Woke] {
            assert_eq!(
                server_after(was, &Fact::Sleep(edge)),
                was,
                "{was:?} {edge:?}"
            );
        }
        assert_eq!(server_after(was, &Fact::Tick), was, "{was:?}");
    }
}

#[test]
fn a_percent_is_0_to_100() {
    assert_eq!(Percent::new(0).map(Percent::get), Some(0));
    assert_eq!(Percent::new(100), Some(Percent::FULL));
    assert_eq!(Percent::FULL.get(), 100);
    assert_eq!(Percent::new(101), None);
}

#[test]
fn the_prefix_is_the_address_in_capitals_with_underscores() {
    let device = "02:00:00:00:00:0a".parse().unwrap();
    assert_eq!(sink_prefix(device), "bluez_output.02_00_00_00_00_0A");
}

#[test]
fn the_card_uses_the_same_device_suffix_as_the_sink() {
    let suffix = DEVICE.replace(':', "_");
    assert_eq!(card_name(device()), format!("bluez_card.{suffix}"));
    assert_eq!(sink_prefix(device()), format!("bluez_output.{suffix}"));
}
