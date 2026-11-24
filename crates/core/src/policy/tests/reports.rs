//! Support tests: what the media adapter's reports do to the ear rules, the players a pause paused and a pause the closed channel cut.

use super::*;

const CUT: Step = Step::Cut;

#[rustfmt::skip]
const CUT_ROWS: [EarRow; 13] = [
    (ONE, OURS, &[IN, ONE_OUT, DOWN, CUT, UP, SINK_OURS], &[PAUSE], &[PAUSE], 0),
    (ONE, OURS, &[IN, ONE_OUT, DOWN, UP, CUT, LATER, SINK_OURS], &[PAUSE], &[PAUSE], 0),
    (ONE, OURS, &[IN, ONE_OUT, DOWN, UP, SINK_OURS, CUT], &[PAUSE], &[PAUSE], 0),
    (ONE, OURS, &[IN, ONE_OUT, DOWN, UP, SINK_ELSEWHERE, CUT, LATER, SINK_OURS], &[PAUSE], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, DOWN, PAUSED, CUT, UP, SINK_OURS], &[PAUSE], &[PAUSE], 1),
    (BOTH, OURS, &[IN, OUT, SETTLED, DOWN, CUT, UP, SINK_OURS], &[PAUSE], &[PAUSE], 0),
    (ONE, OURS, &[IN, ONE_OUT, DOWN, CUT, SINK_OURS, LATER], &[PAUSE], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, DOWN, CUT, UP, IN, SINK_OURS], &[PAUSE], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, DOWN, UP, IN, SINK_OURS, CUT], &[PAUSE], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, LOST, CUT, UP, SINK_OURS], &[PAUSE], &[], 0),
    (ONE, OURS, &[IN, OUT, DOWN, Step::Prefer(OFF), CUT, UP, SINK_OURS], &[PAUSE], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, DOWN, Step::Prefer(BOTH), CUT, UP, SINK_OURS], &[PAUSE], &[], 0),
    (ONE, OURS, &[IN, ONE_OUT, DOWN, CUT, UP, SINK_OURS, OUT], &[PAUSE, PAUSE], &[], 0),
];

#[test]
fn every_cut_pause_row_asks_what_it_names() {
    assert_ear_rows(&CUT_ROWS);
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
    assert_eq!(
        policy.step(&state, Fact::Paused(Era::default(), twice), Duration::ZERO),
        []
    );
    assert_eq!(policy.ear.paused, [named(0)]);
    let first: Vec<PlayerOwner> = (0..PAUSED_MAX).map(named).collect();
    assert_eq!(
        policy.step(
            &state,
            Fact::Paused(Era::default(), first.clone()),
            Duration::ZERO
        ),
        []
    );
    assert_eq!(policy.ear.paused, first);
    let past = vec![named(PAUSED_MAX)];
    assert_eq!(
        policy.step(&state, Fact::Paused(Era::default(), past), Duration::ZERO),
        []
    );
    assert_eq!(policy.ear.paused, first);
}

/// The era of the pause among `effects`.
fn decided(effects: &[Effect]) -> Era {
    let era = effects.iter().find_map(|effect| {
        if let Effect::Media(MediaRequest::PauseAll(era)) = effect {
            Some(*era)
        } else {
            None
        }
    });
    era.expect("a pause decided")
}

#[test]
fn a_pause_report_from_before_a_loss_is_dropped_after_the_relink() {
    let mut policy = Policy::default();
    let mut state = State {
        connected: true,
        ..State::default()
    };
    let now = Duration::ZERO;
    let one_out = |policy: &mut Policy, state: &mut State| {
        drop(policy.step(state, sink_in(OURS, SinkState::Running), now));
        state.ear = EarReport::from_in_ear(true, true);
        drop(policy.step(state, Fact::Changed, now));
        state.ear = EarReport::from_in_ear(true, false);
        decided(&policy.step(state, Fact::Changed, now))
    };
    let before_loss = one_out(&mut policy, &mut state);
    state.reset_device();
    state.connected = false;
    let lost = Signal::Lost {
        device: device(),
        down_at: Some(now),
    };
    drop(policy.step(&state, Fact::Link(lost), now));
    state.connected = true;
    let up = Signal::Up { device: device() };
    drop(policy.step(&state, Fact::Link(up), now));
    let after_relink = one_out(&mut policy, &mut state);
    assert_ne!(before_loss, after_relink);
    let late = Fact::Paused(before_loss, vec![player()]);
    assert_eq!(policy.step(&state, late, now), []);
    assert_eq!(policy.ear.paused, []);
    let own = Fact::Paused(after_relink, vec![player()]);
    assert_eq!(policy.step(&state, own, now), []);
    assert_eq!(policy.ear.paused, [player()]);
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
    assert_eq!(
        policy.step(&state, sink_in(ELSEWHERE, SinkState::Running), now),
        []
    );
    // The buds are back with the sink elsewhere, so this pause is dropped rather than kept for later.
    assert_eq!(
        policy.step(&state, Fact::Paused(Era::default(), vec![player()]), now),
        []
    );
    assert_eq!(
        policy.step(&state, sink_in(OURS, SinkState::Running), now),
        []
    );
    let play = Effect::Media(MediaRequest::Play(vec![other.clone()]));
    assert_eq!(
        policy.step(&state, Fact::Paused(Era::default(), vec![other]), now),
        [play]
    );
}
