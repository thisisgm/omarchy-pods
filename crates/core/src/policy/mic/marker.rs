//! Phase changes driven by the pods' uplink markers.

use super::{
    AudioRequest, Duration, Effect, Hold, Mic, PendingMarker, Phase, RUNNING_AFTER_PREFIX,
    STOPPED_BY_THE_PODS_LINE, Spent, Stopped, Try, UplinkState, Watch, log,
};

impl Mic {
    /// Running answers a daemon START, including a late answer inside its window.
    pub(super) fn marker(&mut self, marker: UplinkState, now: Duration, effects: &mut Vec<Effect>) {
        self.phase = match (self.phase, marker) {
            (Phase::Starting { since, tried, .. }, UplinkState::Running) => {
                let after = now.saturating_sub(since).as_millis();
                effects.push(Effect::Log(format!("{RUNNING_AFTER_PREFIX}{after} ms")));
                Phase::Running(Hold::Wanted, Watch::new(tried, now))
            }
            (Phase::Stopped(stopped) | Phase::Restoring { stopped, .. }, UplinkState::Running) => {
                let owned = matches!(
                    stopped.pending,
                    PendingMarker::StartUntil(until) | PendingMarker::RetryUntil(until) if now < until
                );
                if !owned {
                    return;
                }
                self.armed = None;
                if matches!(self.phase, Phase::Restoring { .. }) {
                    effects.push(Effect::Audio(AudioRequest::CancelRestore));
                }
                Phase::Running(Hold::Late, Watch::new(Try::FIRST, now))
            }
            (
                Phase::Stopping {
                    since,
                    restore,
                    spent,
                },
                UplinkState::Stopped,
            ) => {
                let after = now.saturating_sub(since).as_millis();
                effects.push(Effect::Log(format!("mic: uplink stopped after {after} ms")));
                let stopped = Stopped {
                    spent,
                    pending: PendingMarker::None,
                };
                self.finish_stop(restore, stopped, now, effects)
            }
            // The stopped marker bypasses the playback gate, but a headset profile still blocks a restore.
            (Phase::Running(_, watch), UplinkState::Stopped) => {
                effects.push(log(STOPPED_BY_THE_PODS_LINE));
                let stopped = Stopped {
                    spent: Some(Spent::Unasked),
                    pending: watch
                        .answer_by()
                        .map_or(PendingMarker::None, PendingMarker::RetryUntil),
                };
                self.restore(stopped, now, effects)
            }
            (mut phase, UplinkState::Stopped) => {
                let Some(stopped) = phase.stopped_mut() else {
                    return;
                };
                if stopped.pending != PendingMarker::Stop {
                    return;
                }
                stopped.pending = PendingMarker::None;
                if !self.asleep && !self.audio.quiet && !self.node.blocked() {
                    self.restore(*stopped, now, effects)
                } else {
                    phase
                }
            }
            (phase, _) => phase,
        };
    }
}
