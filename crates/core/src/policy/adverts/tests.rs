//! Support tests: when the advert monitor is wanted, and the rule that applies an advert, as tables.

use std::time::Duration;

use pods_proto::battery::{Batteries, Charge};
use pods_proto::ear::EarReport;
use pods_proto::keys::{Key, ProximityKeys};
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
    0x07, 0x19, 0x01, 0x27, 0x20, 0x62, 0x88, 0x47, 0x00, 0x00, 0x00,
];
/// The same advert from an AirPods Max on the Lightning port.
const MAX_HEADER: [u8; HEADER_LEN] = [
    0x07, 0x19, 0x01, 0x0a, 0x20, 0x62, 0x88, 0x47, 0x00, 0x00, 0x04,
];
/// Primary 80 charging, secondary 60, case 40.
const PAYLOAD: [u8; SEALED_LEN] = [0x00, 0xd0, 0x3c, 0x28, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const RANDOM: [u8; 3] = [0x40, 0x00, 0x01];
const OTHER_IDENTITY: [u8; 16] = [0x11; 16];
const CONNECTION_AT: usize = 10;
/// Primary left, this bud in the case, and neither bud worn.
const IN_CASE_STATUS: u8 = 0x60;

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

fn worn_with_connection(connection: u8) -> Sighting {
    let mut header = HEADER;
    header[CONNECTION_AT] = connection;
    sighting_from(header, IDENTITY_KEY)
}

fn out_of_ear(in_case: bool) -> Sighting {
    let mut header = HEADER;
    header[5] = if in_case { IN_CASE_STATUS } else { 0x20 };
    if in_case {
        header[8] = 0x08;
    }
    sighting_from(header, IDENTITY_KEY)
}

#[test]
fn a_wear_edge_asks_once_and_steady_wear_never_repeats() {
    let (mut policy, mut state) = listening();
    let mut see = |advert, ms| see_advert(&mut policy, &mut state, advert, ms);
    assert!(!see(ours(), 0), "no out-of-ear advert yet");
    assert!(!see(out_of_ear(false), 1));
    assert!(see(ours(), 2), "the wear edge asks to connect");
    assert!(!see(ours(), 30_002), "steady wear is not a new edge");
}

#[test]
fn only_a_disconnected_wear_edge_asks_to_connect() {
    let (mut policy, mut state) = listening();
    let now = Duration::ZERO;
    for connection in [0x04, 0x05, 0x06, 0x07, 0x09, 0xff, 0x00] {
        assert!(!policy.advert(&mut state, &out_of_ear(false), now));
        assert_eq!(
            policy.advert(&mut state, &worn_with_connection(connection), now),
            connection == 0x00,
            "wear edge with connection byte {connection:#04x}"
        );
        assert!(state.ear.any_in_ear());
    }
}

#[test]
fn wear_edges_are_throttled_until_thirty_seconds_after_the_attempt() {
    let (mut policy, mut state) = listening();
    let mut see = |advert, ms| see_advert(&mut policy, &mut state, advert, ms);
    assert!(!see(out_of_ear(false), 0));
    assert!(see(ours(), 1), "the first edge asks to connect");
    assert!(!see(out_of_ear(false), 29_999));
    assert!(!see(ours(), 30_000), "one millisecond before cooldown");
    assert!(!see(ours(), 30_001), "the blocked edge is spent");
    assert!(!see(out_of_ear(false), 30_001));
    assert!(see(ours(), 30_001), "a new edge at the cooldown boundary");
}

#[test]
fn a_manual_disconnect_holds_until_the_case() {
    let (mut policy, mut state) = listening();
    policy.manual_disconnect();
    let mut see = |advert, ms| policy.advert(&mut state, &advert, Duration::from_millis(ms));
    assert!(!see(out_of_ear(false), 0));
    assert!(!see(ours(), 1), "an early edge is held");
    assert!(!see(out_of_ear(true), 2));
    assert!(see(ours(), 3), "the case clears the hold");
}

#[test]
fn a_manual_disconnect_needs_ten_continuous_seconds_both_out() {
    let (mut policy, mut state) = listening();
    policy.manual_disconnect();
    let mut see = |advert, ms| policy.advert(&mut state, &advert, Duration::from_millis(ms));
    assert!(!see(out_of_ear(false), 0));
    assert!(
        !see(ours(), 9_999),
        "one millisecond short resets the interval"
    );
    assert!(!see(out_of_ear(false), 10_000));
    assert!(!see(ours(), 19_999), "the first interval does not count");
    assert!(!see(out_of_ear(false), 20_000));
    assert!(see(ours(), 30_000), "ten seconds out clears the hold");
}

#[test]
fn the_link_pauses_auto_connect_until_intake_is_listening_again() {
    let (mut policy, mut state) = listening();
    let start = Duration::ZERO;
    let link = Fact::Link(up());
    policy.apply(&mut state, &link);
    drop(policy.step(&state, link, start));
    assert!(!policy.advert(&mut state, &out_of_ear(false), start));
    assert!(!policy.advert(&mut state, &ours(), start));
    drop(policy.step(&state, Fact::Link(link_down()), start));
    assert!(!policy.advert(&mut state, &out_of_ear(false), start));
    assert!(!policy.advert(&mut state, &ours(), start));
    policy.apply(&mut state, &Fact::Monitor(MonitorFact::Registered));
    assert!(!policy.advert(&mut state, &out_of_ear(false), start));
    assert!(policy.advert(&mut state, &ours(), start), "listening again");
}

fn in_open_case(both_charging: bool) -> Sighting {
    let mut header = HEADER;
    header[5] = IN_CASE_STATUS;
    let mut payload = PAYLOAD;
    if both_charging {
        payload[2] |= 0x80;
    }
    let made = Keys {
        identity: IDENTITY_KEY,
        payload: PAYLOAD_KEY,
    };
    let advert = advert_of(header, payload, &made, RANDOM);
    Sighting {
        address: advert.address.parse().unwrap(),
        data: advert.data,
    }
}

#[test]
fn an_open_case_clears_the_hold_only_with_both_buds_charging() {
    for (both_charging, cleared) in [(true, true), (false, false)] {
        let (mut policy, mut state) = listening();
        policy.manual_disconnect();
        let mut see = |advert, ms| policy.advert(&mut state, &advert, Duration::from_millis(ms));
        assert!(!see(out_of_ear(false), 0));
        assert!(!see(in_open_case(both_charging), 1));
        assert_eq!(see(ours(), 2), cleared, "both charging: {both_charging}");
    }
}

#[test]
fn a_monitor_gap_forgets_the_wear_seen_before_it() {
    let (mut policy, mut state) = listening();
    assert!(!policy.advert(&mut state, &out_of_ear(false), Duration::ZERO));
    let gap = Fact::Monitor(MonitorFact::Unavailable);
    policy.apply(&mut state, &gap);
    drop(policy.step(&state, gap, Duration::ZERO));
    policy.apply(&mut state, &Fact::Monitor(MonitorFact::Registered));
    assert!(
        !policy.advert(&mut state, &ours(), Duration::from_millis(1)),
        "no edge across the gap"
    );
}

#[test]
fn a_link_between_out_and_worn_adverts_is_not_a_wear_edge() {
    let (mut policy, mut state) = listening();
    let now = Duration::ZERO;
    assert!(!policy.advert(&mut state, &out_of_ear(false), now));
    let link = Fact::Link(up());
    policy.apply(&mut state, &link);
    drop(policy.step(&state, link, now));
    drop(policy.step(&state, Fact::Link(link_down()), now));
    policy.apply(&mut state, &Fact::Monitor(MonitorFact::Registered));
    assert!(
        !policy.advert(&mut state, &ours(), now),
        "no edge across a link"
    );
}

#[test]
fn a_payload_key_change_between_adverts_is_not_a_wear_edge() {
    let (mut policy, mut state) = listening();
    let now = Duration::ZERO;
    assert!(!policy.advert(&mut state, &out_of_ear(false), now));
    let renewed = Keys {
        identity: IDENTITY_KEY,
        payload: [0x22; 16],
    };
    let mut proximity_keys = keys();
    proximity_keys.payload = Key::from_bytes(renewed.payload);
    drop(policy.step(&state, Fact::Link(Signal::Keys(proximity_keys)), now));
    let worn = advert_of(HEADER, PAYLOAD, &renewed, RANDOM);
    let sighting = Sighting {
        address: worn.address.parse().unwrap(),
        data: worn.data,
    };
    assert!(
        !policy.advert(&mut state, &sighting, now),
        "no edge across a key change"
    );
}

#[test]
fn a_link_after_a_manual_disconnect_ends_the_hold() {
    let (mut policy, mut state) = listening();
    policy.manual_disconnect();
    assert!(!policy.advert(&mut state, &out_of_ear(false), Duration::ZERO));
    assert!(!policy.advert(&mut state, &ours(), Duration::ZERO));
    let link = Fact::Link(up());
    policy.apply(&mut state, &link);
    drop(policy.step(&state, link, Duration::ZERO));
    drop(policy.step(&state, Fact::Link(link_down()), Duration::ZERO));
    policy.apply(&mut state, &Fact::Monitor(MonitorFact::Registered));
    assert!(!policy.advert(&mut state, &out_of_ear(false), Duration::ZERO));
    assert!(
        policy.advert(&mut state, &ours(), Duration::from_millis(1)),
        "the hold ended with the link"
    );
}

fn listening() -> (Policy, State) {
    let policy = Policy::new(Some(keys()));
    let state = applied(&policy, &Fact::Monitor(MonitorFact::Registered));
    (policy, state)
}

fn see_advert(policy: &mut Policy, state: &mut State, sighting: Sighting, ms: u64) -> bool {
    let now = Duration::from_millis(ms);
    let connect = policy.advert(state, &sighting, now);
    if connect {
        policy.connect_attempted(now);
    }
    connect
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

fn advertised(mut policy: Policy, sighting: Sighting) -> State {
    let mut state = State::default();
    assert!(!policy.advert(&mut state, &sighting, Duration::ZERO));
    state
}

#[test]
fn an_advert_from_our_pods_sets_what_the_cpp_sets() {
    let state = advertised(Policy::new(Some(keys())), ours());
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
        assert_eq!(advertised(policy, sighting), State::default(), "{row}");
    }
}

#[test]
fn a_headset_takes_its_one_battery_and_no_case() {
    let state = advertised(
        Policy::new(Some(keys())),
        sighting_from(MAX_HEADER, IDENTITY_KEY),
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
