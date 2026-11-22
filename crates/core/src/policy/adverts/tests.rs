//! Support tests: when the advert monitor is wanted, and the rule that applies an advert, as tables.

use std::time::Duration;

use pods_proto::battery::{Batteries, Charge};
use pods_proto::ear::EarReport;
use pods_proto::keys::ProximityKeys;
use pods_proto::model::Model;
use pods_samples::advert::{HEADER_LEN, Keys, SEALED_LEN, advert_of};
use pods_samples::{IDENTITY_KEY, PAYLOAD_KEY};

use super::{MonitorFact, Sighting};
use crate::link::Signal;
use crate::policy::sleep::RESUME_SETTLE_MS;
use crate::policy::tests::{device, sample_keys as keys};
use crate::policy::{Effect, Fact, Policy, SleepEdge};
use crate::state::{AdvertIntake, LidState, State};

/// Paired AirPods Pro 3, the left bud primary and in the case, the right one in an ear, the case at 70 percent and charging, the lid open.
const HEADER: [u8; HEADER_LEN] = [
    0x07, 0x19, 0x01, 0x27, 0x20, 0x62, 0x88, 0x47, 0x00, 0x00, 0x04,
];
/// The same advert from an AirPods Max on the Lightning port.
const MAX_HEADER: [u8; HEADER_LEN] = [
    0x07, 0x19, 0x01, 0x0a, 0x20, 0x62, 0x88, 0x47, 0x00, 0x00, 0x04,
];
/// Primary 80 charging, secondary 60, case 40.
const PAYLOAD: [u8; SEALED_LEN] = [0x00, 0xd0, 0x3c, 0x28, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const RANDOM: [u8; 3] = [0x40, 0x00, 0x01];
const OTHER_IDENTITY: [u8; 16] = [0x11; 16];

fn sighting_from(header: [u8; HEADER_LEN], identity: [u8; 16]) -> Sighting {
    let made = Keys {
        identity,
        payload: PAYLOAD_KEY,
    };
    let advert = advert_of(header, PAYLOAD, &made, RANDOM);
    Sighting {
        address: advert.address.parse().unwrap(),
        data: advert.data,
    }
}

fn ours() -> Sighting {
    sighting_from(HEADER, IDENTITY_KEY)
}

fn charge(level: u8, charging: bool) -> Charge {
    Charge { level, charging }
}

fn up() -> Signal {
    Signal::Up { device: device() }
}

fn link_down() -> Signal {
    Signal::Down
}

fn lost() -> Signal {
    Signal::Lost {
        device: device(),
        down_at: None,
    }
}

/// A name, the keys held at start, the facts stepped, and the monitor and keys effects of the last one.
type Row = (&'static str, Option<ProximityKeys>, Vec<Fact>, Vec<Effect>);

/// The state after `policy` applies `fact` to a fresh one.
fn applied(policy: &Policy, fact: &Fact) -> State {
    let mut state = State::default();
    policy.apply(&mut state, fact);
    state
}

#[test]
fn an_advert_from_our_pods_sets_what_the_cpp_sets() {
    let state = applied(&Policy::new(Some(keys())), &Fact::Advert(ours()));
    let wanted = State {
        model: Model::AirPodsPro3,
        batteries: Batteries {
            left: Some(charge(80, true)),
            right: Some(charge(60, false)),
            case: Some(charge(70, true)),
            headset: None,
        },
        primary: Some(pods_proto::battery::Component::Left),
        ear: EarReport::from_in_ear(false, true),
        lid_state: LidState::Open,
        ..State::default()
    };
    assert_eq!(state, wanted);
}

#[test]
fn an_advert_that_does_not_resolve_parse_or_find_keys_changes_nothing() {
    let mut not_proximity = ours();
    not_proximity.data[0] = 0x10;
    let mut linked = Policy::new(Some(keys()));
    drop(linked.step(&State::default(), Fact::Link(up()), Duration::ZERO));
    let rows = [
        (
            "another identity key",
            Policy::new(Some(keys())),
            sighting_from(HEADER, OTHER_IDENTITY),
        ),
        (
            "not a proximity advert",
            Policy::new(Some(keys())),
            not_proximity,
        ),
        ("no keys held", Policy::default(), ours()),
        ("the link up", linked, ours()),
    ];
    for (row, policy, sighting) in rows {
        assert_eq!(
            applied(&policy, &Fact::Advert(sighting)),
            State::default(),
            "{row}"
        );
    }
}

#[test]
fn a_headset_takes_its_one_battery_and_no_case() {
    let state = applied(
        &Policy::new(Some(keys())),
        &Fact::Advert(sighting_from(MAX_HEADER, IDENTITY_KEY)),
    );
    assert_eq!(state.model, Model::AirPodsMaxLightning);
    let headset_only = Batteries {
        headset: Some(charge(80, true)),
        ..Batteries::default()
    };
    assert_eq!(state.batteries, headset_only);
}

#[test]
fn the_monitor_is_wanted_while_keys_are_held_and_the_link_is_not_up() {
    let monitor = |wanted| vec![Effect::Monitor(wanted)];
    let keyed = || Fact::Link(Signal::Keys(keys()));
    let down = State::default();
    let rows: [Row; 7] = [
        (
            "keys at start, the link up",
            Some(keys()),
            vec![Fact::Link(up())],
            monitor(false),
        ),
        (
            "then down, the device still held",
            Some(keys()),
            vec![Fact::Link(up()), Fact::Link(link_down())],
            monitor(true),
        ),
        (
            "then the device reset",
            Some(keys()),
            vec![
                Fact::Link(up()),
                Fact::Link(link_down()),
                Fact::Link(lost()),
            ],
            Vec::new(),
        ),
        (
            "lost with none wanted yet",
            None,
            vec![Fact::Link(lost())],
            Vec::new(),
        ),
        (
            "no keys, the link up",
            None,
            vec![Fact::Link(up())],
            Vec::new(),
        ),
        (
            "keys while the link is down",
            None,
            vec![keyed()],
            vec![Effect::StoreKeys(keys()), Effect::Monitor(true)],
        ),
        (
            "keys again while the link is down",
            Some(keys()),
            vec![keyed()],
            vec![Effect::StoreKeys(keys())],
        ),
    ];
    for (row, held, facts, wanted) in rows {
        let mut policy = Policy::new(held);
        let mut last = Vec::new();
        for fact in facts {
            last = policy.step(&down, fact, Duration::ZERO);
        }
        let last: Vec<Effect> = last
            .into_iter()
            .filter(|effect| matches!(effect, Effect::Monitor(_) | Effect::StoreKeys(_)))
            .collect();
        assert_eq!(last, wanted, "{row}");
    }
    assert_eq!(Policy::new(Some(keys())).start(), monitor(true));
    assert_eq!(Policy::default().start(), []);
}

#[test]
fn keys_that_arrive_with_the_link_up_ask_for_nothing_more() {
    let mut policy = Policy::default();
    drop(policy.step(&State::default(), Fact::Link(up()), Duration::ZERO));
    let effects = policy.step(
        &State::default(),
        Fact::Link(Signal::Keys(keys())),
        Duration::ZERO,
    );
    assert_eq!(effects, [Effect::StoreKeys(keys())]);
    let effects = policy.step(&State::default(), Fact::Link(link_down()), Duration::ZERO);
    assert!(effects.contains(&Effect::Monitor(true)), "{effects:?}");
}

#[test]
fn a_wake_asks_for_the_monitor_again_only_while_it_is_wanted() {
    let settle = Duration::from_millis(RESUME_SETTLE_MS);
    let before = settle.saturating_sub(Duration::from_millis(1));
    for (held, wanted) in [
        (Some(keys()), vec![Effect::Wake, Effect::Monitor(true)]),
        (None, vec![Effect::Wake]),
    ] {
        let mut policy = Policy::new(held);
        drop(policy.step(
            &State::default(),
            Fact::Sleep(SleepEdge::Woke),
            Duration::ZERO,
        ));
        assert_eq!(policy.step(&State::default(), Fact::Tick, before), []);
        assert_eq!(policy.step(&State::default(), Fact::Tick, settle), wanted);
    }
}

#[test]
fn the_intake_reads_the_monitor_while_wanted_and_paused_from_the_link_up() {
    let intake = |policy: &Policy, fact: Fact| applied(policy, &fact).advert_intake;
    let keyed = Policy::new(Some(keys()));
    assert_eq!(
        intake(&keyed, Fact::Monitor(MonitorFact::Registered)),
        AdvertIntake::Listening
    );
    assert_eq!(
        intake(&keyed, Fact::Monitor(MonitorFact::Unavailable)),
        AdvertIntake::Unavailable
    );
    let unavailable = State {
        advert_intake: AdvertIntake::Unavailable,
        ..State::default()
    };
    let mut linked = Policy::new(Some(keys()));
    drop(linked.step(&State::default(), Fact::Link(up()), Duration::ZERO));
    for (row, policy) in [("no keys", Policy::default()), ("the link up", linked)] {
        let mut state = unavailable.clone();
        policy.apply(&mut state, &Fact::Monitor(MonitorFact::Registered));
        assert_eq!(state, unavailable, "{row}");
    }
    let mut listening = State {
        advert_intake: AdvertIntake::Listening,
        ..State::default()
    };
    keyed.apply(&mut listening, &Fact::Link(up()));
    assert_eq!(listening.advert_intake, AdvertIntake::Paused);
}
