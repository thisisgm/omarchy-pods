//! Support tests: the disconnect notice rule, the low battery latch, and their text and numbers pinned to the C++ daemon's.

use cpp_source::{LOW_BATTERY, MAIN};
use pods_proto::addr::BdAddr;
use pods_proto::battery::Charge;

use super::battery::{LOW_BATTERY_RESET, LOW_BATTERY_TRIGGER, Latch};
use super::notices::{Notice, Part, disconnected};
use super::{Effect, Fact, Policy};
use crate::link::Signal;
use crate::state::State;

/// A synthetic address, never a real one.
const DEVICE: &str = "02:00:00:00:00:01";

fn device() -> BdAddr {
    DEVICE.parse().unwrap()
}

/// The text after each `opener` up to the next quote; sample input `notify(\n tr("AirPods Disconnected"),\n tr("Your AirPods have been disconnected")` with opener `tr("`.
fn quoted<'text>(text: &'text str, opener: &str) -> Vec<&'text str> {
    text.split(opener)
        .skip(1)
        .map(|piece| piece.split_once('"').unwrap().0)
        .collect()
}

/// A latch that already announced a low level.
fn notified() -> Latch {
    let mut latch = Latch::default();
    assert_eq!(latch.evaluate(Some(draining(0))), Some(0));
    latch
}

fn draining(level: u8) -> Charge {
    Charge {
        level,
        charging: false,
    }
}

/// Whether `latch` would announce the next low level.
fn armed(latch: &mut Latch) -> bool {
    latch.evaluate(Some(draining(0))).is_some()
}

#[test]
fn a_device_lost_after_its_link_was_up_shows_the_disconnect_notice() {
    let lost = Signal::Lost {
        device: device(),
        populated: true,
    };
    assert_eq!(disconnected(lost), Some(Notice::Disconnected));
}

#[test]
fn a_device_lost_before_its_link_came_up_shows_nothing() {
    let lost = Signal::Lost {
        device: device(),
        populated: false,
    };
    assert_eq!(disconnected(lost), None);
}

#[test]
fn the_link_coming_up_shows_nothing() {
    let up = Signal::Up { device: device() };
    assert_eq!(disconnected(up), None);
}

#[test]
fn the_disconnect_text_is_the_cpp_daemons() {
    let call = MAIN.between("if (!m_isSuspending) {", ");");
    let notice = Notice::Disconnected;
    assert_eq!(quoted(call, "tr(\""), [notice.title(), notice.message()]);
}

#[test]
fn the_low_battery_text_is_the_cpp_daemons() {
    let labels = quoted(MAIN.between("const Source sources[] = {", "};"), "{ \"");
    let call = MAIN.between("if (fire) {", "*fire));");
    let [title, message] = quoted(call, "tr(\"")[..] else {
        panic!("not a title and a message: {call}");
    };
    let parts = [Part::Left, Part::Right, Part::Case];
    assert_eq!(labels.len(), parts.len(), "{labels:?}");
    for (part, label) in parts.into_iter().zip(labels) {
        let notice = Notice::LowBattery {
            part,
            percent: LOW_BATTERY_TRIGGER,
        };
        assert_eq!(notice.title(), title.replace("%1", label));
        let percent = LOW_BATTERY_TRIGGER.to_string();
        assert_eq!(notice.message(), message.replace("%1", &percent));
    }
}

#[test]
fn the_latch_levels_are_the_cpp_daemons() {
    let trigger = LOW_BATTERY.find(Some("LowBatteryLatch"), "quint8 trigger = ", ',');
    let reset = LOW_BATTERY.find(Some("LowBatteryLatch"), "quint8 reset = ", ')');
    assert_eq!(trigger, LOW_BATTERY_TRIGGER.to_string());
    assert_eq!(reset, LOW_BATTERY_RESET.to_string());
}

#[test]
fn every_level_in_both_charging_states_moves_the_latch_as_the_cpp_one() {
    for level in 0..=100 {
        for charging in [false, true] {
            let reading = Some(Charge { level, charging });
            let mut fresh = Latch::default();
            let fired = fresh.evaluate(reading);
            let low = matches!(level, 0..=10);
            assert_eq!(fired, (low && !charging).then_some(level), "{reading:?}");
            assert_eq!(
                armed(&mut fresh),
                !low || charging,
                "fresh after {reading:?}"
            );
            let mut latched = notified();
            assert_eq!(latched.evaluate(reading), None, "latched, {reading:?}");
            let rearmed = charging || matches!(level, 15..=100);
            assert_eq!(armed(&mut latched), rearmed, "latched after {reading:?}");
        }
    }
}

#[test]
fn no_reading_fires_nothing_and_changes_nothing() {
    let mut fresh = Latch::default();
    assert_eq!(fresh.evaluate(None), None);
    assert!(armed(&mut fresh));
    let mut latched = notified();
    assert_eq!(latched.evaluate(None), None);
    assert!(!armed(&mut latched));
}

#[test]
fn the_policy_answers_a_lost_link_and_a_low_case_and_keeps_the_latch_past_the_loss() {
    let low_case = Some(draining(LOW_BATTERY_TRIGGER));
    let mut policy = Policy::default();
    let mut state = State::default();
    state.batteries.case = low_case;
    let case_notice = Notice::LowBattery {
        part: Part::Case,
        percent: LOW_BATTERY_TRIGGER,
    };
    assert_eq!(
        policy.step(&state, Fact::Changed),
        [Effect::Notify(case_notice)]
    );
    state.reset_device();
    let lost = Signal::Lost {
        device: device(),
        populated: true,
    };
    assert_eq!(
        policy.step(&state, Fact::Link(lost)),
        [Effect::Notify(Notice::Disconnected)]
    );
    assert_eq!(policy.step(&state, Fact::Changed), []);
    state.batteries.case = low_case;
    assert_eq!(policy.step(&state, Fact::Changed), []);
}
