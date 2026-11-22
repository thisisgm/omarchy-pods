//! Support tests: the suspend window's edges, its settle and which losses it covers, and the settle pinned to the C++ daemon's.

use std::time::Duration;

use cpp_source::MAIN;

use super::{RESUME_SETTLE_MS, SleepEdge, Suspend};

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// A window opened at `sleep` and woken at `wake`, both in milliseconds.
fn woken(sleep: u64, wake: u64) -> Suspend {
    let mut suspend = Suspend::default();
    suspend.edge(SleepEdge::Sleeping, ms(sleep));
    suspend.edge(SleepEdge::Woke, ms(wake));
    suspend
}

#[test]
fn the_settle_is_the_cpp_daemons() {
    let settle = MAIN.find(Some("void onSystemWakingUp()"), "QTimer::singleShot(", ',');
    assert_eq!(settle, RESUME_SETTLE_MS.to_string());
}

#[test]
fn no_suspend_covers_nothing_and_waits_on_nothing() {
    let mut suspend = Suspend::default();
    assert!(!suspend.covers(Duration::ZERO));
    assert_eq!(suspend.deadline(), None);
    assert!(!suspend.close(ms(u64::MAX)));
}

#[test]
fn a_sleep_with_no_wake_covers_every_later_moment() {
    let mut suspend = Suspend::default();
    suspend.edge(SleepEdge::Sleeping, ms(100));
    assert!(!suspend.covers(ms(99)));
    assert!(suspend.covers(ms(100)));
    assert!(suspend.covers(ms(u64::MAX)));
    assert_eq!(suspend.deadline(), None);
    assert!(!suspend.close(ms(u64::MAX)));
}

#[test]
fn the_window_closes_the_settle_after_the_wake_and_not_before() {
    let mut suspend = woken(100, 500);
    let closes = ms(500 + RESUME_SETTLE_MS);
    assert_eq!(suspend.deadline(), Some(closes));
    assert!(!suspend.close(closes.saturating_sub(ms(1))));
    assert!(suspend.close(closes));
    assert_eq!(suspend.deadline(), None);
    assert!(!suspend.close(closes), "a window closes once");
}

#[test]
fn a_closed_window_still_covers_its_span_and_only_that() {
    let mut suspend = woken(100, 500);
    let closes = ms(500 + RESUME_SETTLE_MS);
    assert!(suspend.close(closes));
    assert!(!suspend.covers(ms(99)));
    assert!(suspend.covers(ms(100)));
    assert!(suspend.covers(closes));
    assert!(!suspend.covers(closes.saturating_add(ms(1))));
}

#[test]
fn a_settling_window_covers_up_to_its_close() {
    let suspend = woken(100, 500);
    let closes = ms(500 + RESUME_SETTLE_MS);
    assert!(suspend.covers(closes));
    assert!(!suspend.covers(closes.saturating_add(ms(1))));
}

#[test]
fn two_sleeps_are_one_window_from_the_first() {
    let mut suspend = Suspend::default();
    suspend.edge(SleepEdge::Sleeping, ms(100));
    suspend.edge(SleepEdge::Sleeping, ms(300));
    suspend.edge(SleepEdge::Woke, ms(500));
    assert!(suspend.covers(ms(100)));
    assert_eq!(suspend.deadline(), Some(ms(500 + RESUME_SETTLE_MS)));
}

#[test]
fn a_sleep_while_settling_keeps_the_window_open_until_the_next_wake() {
    let mut suspend = woken(100, 500);
    suspend.edge(SleepEdge::Sleeping, ms(600));
    assert_eq!(suspend.deadline(), None);
    assert!(suspend.covers(ms(100)));
    suspend.edge(SleepEdge::Woke, ms(900));
    assert_eq!(suspend.deadline(), Some(ms(900 + RESUME_SETTLE_MS)));
}

#[test]
fn a_wake_with_no_sleep_seen_opens_a_window_at_the_wake() {
    let mut suspend = Suspend::default();
    suspend.edge(SleepEdge::Woke, ms(500));
    assert!(!suspend.covers(ms(499)));
    assert!(suspend.covers(ms(500)));
    assert_eq!(suspend.deadline(), Some(ms(500 + RESUME_SETTLE_MS)));
}

#[test]
fn a_new_sleep_after_the_link_came_up_past_a_closed_window_opens_a_new_one() {
    let mut suspend = woken(100, 500);
    assert!(suspend.close(ms(500 + RESUME_SETTLE_MS)));
    suspend.relinked();
    assert_eq!(suspend, Suspend::Never);
    suspend.edge(SleepEdge::Sleeping, ms(10_000));
    assert!(!suspend.covers(ms(100)));
    assert!(suspend.covers(ms(10_000)));
}

#[test]
fn the_link_coming_up_forgets_only_a_closed_window() {
    let mut suspend = Suspend::default();
    suspend.edge(SleepEdge::Sleeping, ms(100));
    suspend.relinked();
    assert!(suspend.covers(ms(100)));
    suspend.edge(SleepEdge::Woke, ms(500));
    suspend.relinked();
    assert_eq!(suspend.deadline(), Some(ms(500 + RESUME_SETTLE_MS)));
}

#[test]
fn a_sleep_while_the_link_stayed_down_extends_the_closed_window() {
    let mut suspend = woken(100, 500);
    assert!(suspend.close(ms(500 + RESUME_SETTLE_MS)));
    suspend.edge(SleepEdge::Sleeping, ms(10_000));
    assert!(suspend.covers(ms(100)));
    assert!(suspend.covers(ms(5_000)));
    assert!(!suspend.covers(ms(99)));
    suspend.edge(SleepEdge::Woke, ms(12_000));
    let closes = ms(12_000 + RESUME_SETTLE_MS);
    assert_eq!(suspend.deadline(), Some(closes));
    assert!(suspend.close(closes));
    assert!(suspend.covers(ms(100)));
    assert!(suspend.covers(closes));
    assert!(!suspend.covers(closes.saturating_add(ms(1))));
}

#[test]
fn three_suspends_with_the_link_down_are_one_window_from_the_first() {
    let mut suspend = woken(100, 500);
    assert!(suspend.close(ms(500 + RESUME_SETTLE_MS)));
    suspend.edge(SleepEdge::Sleeping, ms(4_000));
    suspend.edge(SleepEdge::Woke, ms(4_500));
    assert!(suspend.close(ms(4_500 + RESUME_SETTLE_MS)));
    suspend.edge(SleepEdge::Sleeping, ms(7_000));
    suspend.edge(SleepEdge::Woke, ms(7_500));
    let closes = ms(7_500 + RESUME_SETTLE_MS);
    assert!(suspend.close(closes));
    assert!(suspend.covers(ms(100)));
    assert!(suspend.covers(closes));
    assert!(!suspend.covers(closes.saturating_add(ms(1))));
}

#[test]
fn a_lone_wake_while_the_link_stayed_down_extends_the_closed_window() {
    let mut suspend = woken(100, 500);
    assert!(suspend.close(ms(500 + RESUME_SETTLE_MS)));
    suspend.edge(SleepEdge::Woke, ms(10_000));
    assert!(suspend.covers(ms(100)));
    assert_eq!(suspend.deadline(), Some(ms(10_000 + RESUME_SETTLE_MS)));
}
