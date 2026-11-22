//! Support tests: the disconnect notice rule, the low battery latch, the ear rules, and their text and numbers pinned to the C++ daemon's.

use std::time::Duration;

use cpp_source::{LOW_BATTERY, MAIN, MEDIA_CONTROLLER_SOURCE, NOTIFIER};
use pods_proto::addr::BdAddr;
use pods_proto::battery::Charge;
use pods_proto::ear::EarReport;
use pods_proto::keys::{Key, ProximityKeys};
use pods_samples::{IDENTITY_KEY, PAYLOAD_KEY};

use super::audio::{AudioFact, AudioRequest, Percent, SinkFact};
use super::battery::{LOW_BATTERY_RESET, LOW_BATTERY_TRIGGER, Latch};
use super::ear::{BOTH_OUT_SETTLE_MS, BUS_NAME_MAX};
use super::notices::{Notice, Part, disconnected};
use super::sleep::{RESUME_SETTLE_MS, Suspend};
use super::{Effect, Fact, MediaRequest, PAUSED_MAX, PlayerOwner, Policy, SleepEdge};
use crate::link::Signal;
use crate::state::{EarDetectionBehavior, LidState, State};

/// What every lost link asks of the audio adapter, whatever the notices do.
const UNFOLLOW: Effect = Effect::Audio(AudioRequest::Follow(None));

/// A synthetic address, never a real one.
const DEVICE: &str = "02:00:00:00:00:01";

pub(super) fn device() -> BdAddr {
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
        down_at: Some(Duration::ZERO),
    };
    assert_eq!(
        disconnected(&lost, Suspend::default()),
        Some(Notice::Disconnected)
    );
}

#[test]
fn a_device_lost_before_its_link_came_up_shows_nothing() {
    let lost = Signal::Lost {
        device: device(),
        down_at: None,
    };
    assert_eq!(disconnected(&lost, Suspend::default()), None);
}

#[test]
fn the_link_coming_up_shows_nothing() {
    let up = Signal::Up { device: device() };
    assert_eq!(disconnected(&up, Suspend::default()), None);
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
        policy.step(&state, Fact::Changed, Duration::ZERO),
        [Effect::Notify(case_notice)]
    );
    state.reset_device();
    let lost = Signal::Lost {
        device: device(),
        down_at: Some(Duration::ZERO),
    };
    assert_eq!(
        policy.step(&state, Fact::Link(lost), Duration::ZERO),
        [Effect::Notify(Notice::Disconnected), UNFOLLOW]
    );
    assert_eq!(policy.step(&state, Fact::Changed, Duration::ZERO), []);
    state.batteries.case = low_case;
    assert_eq!(policy.step(&state, Fact::Changed, Duration::ZERO), []);
}

#[test]
fn a_loss_that_began_in_a_suspend_is_silent_and_the_wake_comes_once_at_the_settle() {
    let mut policy = Policy::default();
    let state = State::default();
    let at = Duration::from_millis;
    let lost = |down_at| {
        Fact::Link(Signal::Lost {
            device: device(),
            down_at: Some(at(down_at)),
        })
    };
    assert_eq!(
        policy.step(&state, Fact::Sleep(SleepEdge::Sleeping), at(100)),
        []
    );
    assert_eq!(
        policy.step(&state, Fact::Sleep(SleepEdge::Woke), at(500)),
        []
    );
    let closes = 500 + RESUME_SETTLE_MS;
    assert_eq!(policy.deadline(), Some(at(closes)));
    assert_eq!(policy.step(&state, Fact::Tick, at(closes - 1)), []);
    assert_eq!(policy.step(&state, Fact::Tick, at(closes)), [Effect::Wake]);
    assert_eq!(policy.step(&state, Fact::Tick, at(closes)), []);
    assert_eq!(policy.deadline(), None);
    assert_eq!(policy.step(&state, lost(200), at(20_000)), [UNFOLLOW]);
    assert_eq!(
        policy.step(&state, lost(closes + 1), at(20_000)),
        [Effect::Notify(Notice::Disconnected), UNFOLLOW]
    );
    assert_eq!(
        policy.step(&state, lost(99), at(20_000)),
        [Effect::Notify(Notice::Disconnected), UNFOLLOW]
    );
}

#[test]
fn a_second_sleep_keeps_a_loss_awaiting_its_reset_silent_until_the_link_comes_back() {
    let at = Duration::from_millis;
    let down = State::default();
    let up = State {
        connected: true,
        ..State::default()
    };
    let lost = |down_at| {
        Fact::Link(Signal::Lost {
            device: device(),
            down_at: Some(at(down_at)),
        })
    };
    let closes = 500 + RESUME_SETTLE_MS;
    let suspended_once = |at_close: &State| {
        let mut policy = Policy::default();
        assert_eq!(
            policy.step(&up, Fact::Sleep(SleepEdge::Sleeping), at(100)),
            []
        );
        assert_eq!(
            policy.step(&down, Fact::Sleep(SleepEdge::Woke), at(500)),
            []
        );
        assert_eq!(
            policy.step(at_close, Fact::Tick, at(closes)),
            [Effect::Wake]
        );
        policy
    };
    // A loss inside the first window, the link down since, its reset after the second sleep.
    let mut policy = suspended_once(&down);
    assert_eq!(
        policy.step(&down, Fact::Sleep(SleepEdge::Sleeping), at(closes + 1_000)),
        []
    );
    assert_eq!(
        policy.step(&down, lost(200), at(closes + 7_750)),
        [UNFOLLOW]
    );
    // The link back after the first window, then lost before the second sleep: announced.
    let mut policy = suspended_once(&down);
    assert_eq!(policy.step(&up, Fact::Changed, at(closes + 50)), []);
    assert_eq!(policy.step(&down, Fact::Changed, at(closes + 100)), []);
    assert_eq!(
        policy.step(&down, Fact::Sleep(SleepEdge::Sleeping), at(closes + 1_000)),
        []
    );
    assert_eq!(
        policy.step(&down, lost(closes + 100), at(closes + 7_850)),
        [Effect::Notify(Notice::Disconnected), UNFOLLOW]
    );
    // The link up through the first window, hours of it, then lost just before a sleep: announced.
    let hours = 8 * 60 * 60 * 1_000;
    let mut policy = suspended_once(&up);
    assert_eq!(policy.step(&down, Fact::Changed, at(hours)), []);
    assert_eq!(
        policy.step(&down, Fact::Sleep(SleepEdge::Sleeping), at(hours + 5_000)),
        []
    );
    assert_eq!(
        policy.step(&down, lost(hours), at(hours + 7_750)),
        [Effect::Notify(Notice::Disconnected), UNFOLLOW]
    );
}

#[test]
fn the_cpp_notifier_drops_a_notice_only_at_the_send() {
    let send = NOTIFIER.body("void notify(");
    assert!(send.trim_start().starts_with("if (!m_enabled)"), "{send}");
    let check = MAIN.body("void checkLowBatteryThresholds()");
    let evaluated = check.find("latch->evaluate(").unwrap();
    assert!(
        evaluated < check.find("m_notifier->notify(").unwrap(),
        "{check}"
    );
}

#[test]
fn the_notices_and_the_latches_are_the_same_with_notices_off() {
    let lost = Signal::Lost {
        device: device(),
        down_at: Some(Duration::ZERO),
    };
    let steps = |notifications: bool| {
        let mut policy = Policy::default();
        let mut state = State {
            notifications,
            ..State::default()
        };
        let mut effects = Vec::new();
        for case in [LOW_BATTERY_TRIGGER, LOW_BATTERY_RESET, LOW_BATTERY_TRIGGER] {
            state.batteries.case = Some(draining(case));
            effects.push(policy.step(&state, Fact::Changed, Duration::ZERO));
        }
        effects.push(policy.step(&state, Fact::Link(lost.clone()), Duration::ZERO));
        effects
    };
    let on = steps(true);
    assert_eq!(steps(false), on);
    let case_notice = Effect::Notify(Notice::LowBattery {
        part: Part::Case,
        percent: LOW_BATTERY_TRIGGER,
    });
    assert_eq!(on[0], on[2]);
    assert_eq!(on[2], [case_notice]);
    assert_eq!(on[3], [Effect::Notify(Notice::Disconnected), UNFOLLOW]);
}

/// The sample pair the pods answer with.
pub(super) fn sample_keys() -> ProximityKeys {
    ProximityKeys {
        identity: Key::from_bytes(IDENTITY_KEY),
        payload: Key::from_bytes(PAYLOAD_KEY),
    }
}

#[test]
fn every_reply_goes_to_the_writer() {
    let keys = sample_keys();
    let mut policy = Policy::default();
    let up = Fact::Link(Signal::Up { device: device() });
    drop(policy.step(&State::default(), up, Duration::ZERO));
    for _ in 0..2 {
        let reply = Fact::Link(Signal::Keys(keys.clone()));
        let effects = policy.step(&State::default(), reply, Duration::ZERO);
        assert_eq!(effects, [Effect::StoreKeys(keys.clone())]);
    }
}

/// The time between two steps of an ear row.
const STEP_MS: u64 = 100;

/// When both buds leave in the settle tests.
const OUT_AT_MS: u64 = 100;

/// A later change that leaves both buds out.
const LATER_AT_MS: u64 = 900;

/// A wake from suspend inside the settle.
const WOKE_AT_MS: u64 = 200;

/// One step of an ear row, each `STEP_MS` after the last.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// Primary then secondary, true for in an ear.
    Report(bool, bool),
    Prefer(EarDetectionBehavior),
    /// The adapter reports `PLAYER` paused.
    Paused,
    Up,
    /// The clock moves to the end of the pending settle.
    Settle,
    Lost,
    /// The pods' sink becomes the default.
    Ours,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asks {
    PauseAll,
    PlayPlayer,
}

const ONE: EarDetectionBehavior = EarDetectionBehavior::PauseWhenOneRemoved;
const BOTH: EarDetectionBehavior = EarDetectionBehavior::PauseWhenBothRemoved;
const OFF: EarDetectionBehavior = EarDetectionBehavior::Disabled;
const IN: Step = Step::Report(true, true);
const ONE_OUT: Step = Step::Report(true, false);
/// One bud out as `ONE_OUT`, reported after the other bud became the primary.
const OTHER_OUT: Step = Step::Report(false, true);
const OUT: Step = Step::Report(false, false);
const PAUSED: Step = Step::Paused;
const UP: Step = Step::Up;
const SETTLED: Step = Step::Settle;
const LOST: Step = Step::Lost;
const SINK_OURS: Step = Step::Ours;
const PAUSE: Asks = Asks::PauseAll;
const PLAY: Asks = Asks::PlayPlayer;
const OURS: bool = true;
const ELSEWHERE: bool = false;
const PLAYER: &str = ":1.42";

/// Preference, whose sink is the default, the steps, what they ask at once, what the settle's end asks, players still remembered.
type EarRow = (
    EarDetectionBehavior,
    bool,
    &'static [Step],
    &'static [Asks],
    &'static [Asks],
    usize,
);

#[rustfmt::skip]
const EAR_ROWS: [EarRow; 38] = [
    (OFF, OURS, &[IN, ONE_OUT, OUT], &[], &[], 0),
    (OFF, OURS, &[IN, OUT, PAUSED, IN], &[], &[], 0),
    (OFF, OURS, &[IN, PAUSED], &[], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT], &[PAUSE], &[], 0),
    (ONE, OURS, &[IN, OUT], &[PAUSE], &[], 0),
    (ONE, ELSEWHERE, &[IN, ONE_OUT], &[], &[], 0),
    (ONE, OURS, &[ONE_OUT], &[], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, OTHER_OUT], &[PAUSE], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, PAUSED, IN], &[PAUSE, PLAY], &[], 0),
    (ONE, OURS, &[IN, OUT, PAUSED, ONE_OUT], &[PAUSE], &[], 1),
    (ONE, ELSEWHERE, &[IN, ONE_OUT, PAUSED, IN], &[], &[], 0),
    (ONE, ELSEWHERE, &[IN, ONE_OUT, PAUSED, IN, SINK_OURS, ONE_OUT, IN], &[PAUSE], &[], 0),
    (ONE, OURS, &[IN, UP, ONE_OUT], &[], &[], 0),
    (ONE, OURS, &[IN, PAUSED], &[PLAY], &[], 0),
    (BOTH, OURS, &[IN, OUT], &[], &[PAUSE], 0),
    (BOTH, ELSEWHERE, &[IN, OUT], &[], &[], 0),
    (BOTH, OURS, &[IN, ONE_OUT], &[], &[], 0),
    (BOTH, OURS, &[IN, OUT, PAUSED, ONE_OUT], &[PLAY], &[], 0),
    (BOTH, OURS, &[IN, OUT, PAUSED, IN], &[PLAY], &[], 0),
    (BOTH, OURS, &[IN, OUT, Step::Prefer(ONE)], &[], &[], 0),
    (BOTH, OURS, &[IN, OUT, PAUSED, Step::Prefer(OFF)], &[], &[], 1),
    (BOTH, OURS, &[IN, OUT, PAUSED, Step::Prefer(OFF), IN, Step::Prefer(ONE), ONE_OUT, IN], &[PAUSE], &[], 0),
    (BOTH, OURS, &[IN, OUT, PAUSED, LOST], &[], &[], 0),
    (BOTH, OURS, &[IN, OUT, LOST, SINK_OURS], &[], &[], 0),
    (BOTH, OURS, &[IN, LOST, SINK_OURS], &[], &[], 0),
    (BOTH, OURS, &[IN, OUT, SETTLED, PAUSED, Step::Prefer(OFF), IN, Step::Prefer(BOTH), OUT, IN], &[PAUSE], &[], 0),
    (BOTH, ELSEWHERE, &[IN, OUT, PAUSED, IN, SINK_OURS, ONE_OUT, OUT, IN], &[], &[], 0),
    (BOTH, OURS, &[IN, OUT, SETTLED, PAUSED, Step::Prefer(OFF), ONE_OUT, Step::Prefer(BOTH), OUT, SETTLED, ONE_OUT], &[PAUSE, PAUSE], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, PAUSED, Step::Prefer(BOTH), OUT, SETTLED, IN], &[PAUSE, PAUSE, PLAY], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, PAUSED, OUT, ONE_OUT, OUT, IN], &[PAUSE, PAUSE, PAUSE, PLAY], &[], 0),
    (BOTH, OURS, &[IN, OUT, SETTLED, PAUSED, Step::Prefer(OFF), ONE_OUT, IN, Step::Prefer(BOTH), OUT, SETTLED, IN], &[PAUSE, PAUSE], &[], 0),
    (BOTH, OURS, &[IN, OUT, SETTLED, PAUSED, Step::Prefer(OFF), IN, Step::Prefer(BOTH), OUT, SETTLED, IN], &[PAUSE, PAUSE], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, LOST, PAUSED], &[PAUSE], &[], 0),
    (BOTH, ELSEWHERE, &[IN, OUT, PAUSED, ONE_OUT, Step::Prefer(ONE), SINK_OURS, IN], &[], &[], 0),
    (ONE, OURS, &[IN, OUT, PAUSED, Step::Prefer(OFF), ONE_OUT, Step::Prefer(ONE), IN], &[PAUSE], &[], 0),
    (ONE, OURS, &[IN, OUT, Step::Prefer(OFF), ONE_OUT, Step::Prefer(ONE), PAUSED, OUT, IN], &[PAUSE, PAUSE, PLAY], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, PAUSED, Step::Prefer(BOTH), IN, OUT, SETTLED, IN], &[PAUSE, PLAY, PAUSE], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, PAUSED, Step::Prefer(OFF), IN, Step::Prefer(ONE), ONE_OUT, IN], &[PAUSE, PAUSE], &[], 0),
];

fn player() -> PlayerOwner {
    PlayerOwner::new(PLAYER).unwrap()
}

fn asks(effects: Vec<Effect>) -> Vec<Asks> {
    let mut asked = Vec::new();
    for effect in effects {
        match effect {
            Effect::Media(MediaRequest::PauseAll) => asked.push(PAUSE),
            Effect::Media(MediaRequest::Play(players)) => {
                assert_eq!(players, [player()]);
                asked.push(PLAY);
            }
            Effect::Notify(_)
            | Effect::Wake
            | Effect::Audio(_)
            | Effect::StoreKeys(_)
            | Effect::Monitor(_) => {}
        }
    }
    asked
}

fn sink(is_default: bool) -> Fact {
    let volume = Percent::FULL;
    Fact::Audio(AudioFact::Sink(Some(SinkFact { is_default, volume })))
}

/// Runs a row's steps on a linked state, then the clock to the settle's end if one is pending.
fn run_ear_row(row: EarRow) -> (Vec<Asks>, Vec<Asks>, usize) {
    let (behavior, ours, steps, ..) = row;
    let mut policy = Policy::default();
    let mut state = State {
        connected: true,
        ear_detection_behavior: behavior,
        ..State::default()
    };
    let mut now = Duration::ZERO;
    let mut at_once = asks(policy.step(&state, Fact::Changed, now));
    at_once.extend(asks(policy.step(&state, sink(ours), now)));
    for &step in steps {
        now = now.saturating_add(Duration::from_millis(STEP_MS));
        let fact = match step {
            Step::Report(primary, secondary) => {
                state.ear = EarReport::from_in_ear(primary, secondary);
                Fact::Changed
            }
            Step::Prefer(behavior) => {
                state.ear_detection_behavior = behavior;
                Fact::Changed
            }
            Step::Paused => Fact::Paused(vec![player()]),
            Step::Ours => sink(OURS),
            Step::Settle => {
                now = policy.deadline().expect("a settle pending");
                Fact::Tick
            }
            Step::Up => {
                let up = Signal::Up { device: device() };
                at_once.extend(asks(policy.step(&state, Fact::Link(up), now)));
                Fact::Changed
            }
            Step::Lost => {
                state.reset_device();
                state.connected = false;
                let down_at = Some(now);
                let lost = Signal::Lost {
                    device: device(),
                    down_at,
                };
                at_once.extend(asks(policy.step(&state, Fact::Link(lost), now)));
                Fact::Changed
            }
        };
        at_once.extend(asks(policy.step(&state, fact, now)));
    }
    let mut at_settle = Vec::new();
    if let Some(at) = policy.deadline() {
        let before = at.saturating_sub(Duration::from_millis(1));
        assert_eq!(asks(policy.step(&state, Fact::Tick, before)), []);
        at_settle = asks(policy.step(&state, Fact::Tick, at));
        assert_eq!(policy.deadline(), None);
    }
    (at_once, at_settle, policy.ear.paused.len())
}

#[test]
fn every_ear_row_asks_what_it_names() {
    for row in EAR_ROWS {
        let (.., at_once, at_settle, remembered) = row;
        let ran = run_ear_row(row);
        assert_eq!(
            ran,
            (at_once.to_vec(), at_settle.to_vec(), remembered),
            "{row:?}"
        );
    }
}

#[test]
fn the_settle_ends_at_its_first_both_out_report_and_is_the_cpp_daemons() {
    let source = MEDIA_CONTROLLER_SOURCE.find(None, "bothPodsOutSettleMs = ", ';');
    assert_eq!(source, BOTH_OUT_SETTLE_MS.to_string());
    let mut policy = Policy::default();
    let mut state = State {
        connected: true,
        ear_detection_behavior: BOTH,
        ..State::default()
    };
    let at = Duration::from_millis;
    state.ear = EarReport::from_in_ear(true, true);
    assert_eq!(policy.step(&state, Fact::Changed, at(0)), []);
    state.ear = EarReport::from_in_ear(false, false);
    assert_eq!(policy.step(&state, Fact::Changed, at(OUT_AT_MS)), []);
    // A change that leaves both buds out must not move the settle.
    state.lid_state = LidState::Open;
    assert_eq!(policy.step(&state, Fact::Changed, at(LATER_AT_MS)), []);
    assert_eq!(policy.deadline(), Some(at(OUT_AT_MS + BOTH_OUT_SETTLE_MS)));
}

#[test]
fn the_settle_and_a_suspend_window_each_end_at_their_own_deadline() {
    let mut policy = Policy::default();
    let mut state = State {
        connected: true,
        ear_detection_behavior: BOTH,
        ..State::default()
    };
    let at = Duration::from_millis;
    assert_eq!(policy.step(&state, sink(OURS), at(0)), []);
    state.ear = EarReport::from_in_ear(true, true);
    assert_eq!(policy.step(&state, Fact::Changed, at(0)), []);
    state.ear = EarReport::from_in_ear(false, false);
    assert_eq!(policy.step(&state, Fact::Changed, at(OUT_AT_MS)), []);
    let woke = Fact::Sleep(SleepEdge::Woke);
    assert_eq!(policy.step(&state, woke, at(WOKE_AT_MS)), []);
    let settled = at(OUT_AT_MS + BOTH_OUT_SETTLE_MS);
    let closes = at(WOKE_AT_MS + RESUME_SETTLE_MS);
    assert_eq!(policy.deadline(), Some(settled));
    let pause_all = Effect::Media(MediaRequest::PauseAll);
    assert_eq!(policy.step(&state, Fact::Tick, settled), [pause_all]);
    assert_eq!(policy.deadline(), Some(closes));
    assert_eq!(policy.step(&state, Fact::Tick, closes), [Effect::Wake]);
    assert_eq!(policy.deadline(), None);
}

#[test]
fn players_past_the_bound_and_repeated_ones_are_not_remembered() {
    let mut policy = Policy::default();
    let state = State {
        connected: true,
        ..State::default()
    };
    let named = |index: usize| PlayerOwner::new(&format!("{PLAYER}{index}")).unwrap();
    let twice = vec![named(0), named(0)];
    assert_eq!(policy.step(&state, Fact::Paused(twice), Duration::ZERO), []);
    assert_eq!(policy.ear.paused, [named(0)]);
    let first: Vec<PlayerOwner> = (0..PAUSED_MAX).map(named).collect();
    assert_eq!(
        policy.step(&state, Fact::Paused(first.clone()), Duration::ZERO),
        []
    );
    assert_eq!(policy.ear.paused, first);
    let past = vec![named(PAUSED_MAX)];
    assert_eq!(policy.step(&state, Fact::Paused(past), Duration::ZERO), []);
    assert_eq!(policy.ear.paused, first);
}

#[test]
fn a_late_pause_with_the_buds_back_plays_only_its_own_players() {
    let mut policy = Policy::default();
    let mut state = State {
        connected: true,
        ear_detection_behavior: ONE,
        ..State::default()
    };
    state.ear = EarReport::from_in_ear(true, true);
    let now = Duration::ZERO;
    let other = PlayerOwner::new(&format!("{PLAYER}2")).unwrap();
    assert_eq!(policy.step(&state, Fact::Changed, now), []);
    assert_eq!(policy.step(&state, sink(ELSEWHERE), now), []);
    // The buds are back with the sink elsewhere, so this pause is dropped rather than kept for later.
    assert_eq!(policy.step(&state, Fact::Paused(vec![player()]), now), []);
    assert_eq!(policy.step(&state, sink(OURS), now), []);
    let play = Effect::Media(MediaRequest::Play(vec![other.clone()]));
    assert_eq!(policy.step(&state, Fact::Paused(vec![other]), now), [play]);
}

#[test]
fn a_player_owner_is_a_unique_bus_name() {
    let longest = format!("{PLAYER}{}", "2".repeat(BUS_NAME_MAX - PLAYER.len()));
    assert_eq!(PlayerOwner::new(&longest).unwrap().as_str(), longest);
    let past = format!("{longest}2");
    for refused in ["", ":", "org.mpris.MediaPlayer2.one", "1.42", &past] {
        assert_eq!(PlayerOwner::new(refused), None, "{refused:?}");
    }
}
