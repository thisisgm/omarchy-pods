//! Chooses the microphone path once for each span with a recorder on the node.

use std::time::Duration;

use super::audio::Recorders;

pub const DICTATION_RECORDERS: [&str; 2] = ["superwhisper-capture", "superwhisper-history"];
/// The audio server can report its streams just after the node starts running.
pub const ROUTE_WAIT_MS: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Dictation,
    Uplink,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) enum Edge {
    #[default]
    None,
    Waiting(Duration),
    Decided(Route),
}

impl Edge {
    pub(super) fn step(
        &mut self,
        recording: bool,
        recorders: Recorders,
        fresh: bool,
        armed: bool,
        now: Duration,
    ) -> Option<Route> {
        if !recording {
            *self = Self::None;
            return None;
        }
        if let Self::Decided(route) = *self {
            return Some(route);
        }
        if !fresh && self.deadline().is_none_or(|at| now < at) {
            if matches!(self, Self::None) {
                *self = Self::Waiting(now);
            }
            return None;
        }
        let route = if recorders.other == 0 && armed {
            Route::Dictation
        } else {
            Route::Uplink
        };
        *self = Self::Decided(route);
        Some(route)
    }

    pub(super) fn deadline(self) -> Option<Duration> {
        match self {
            Self::Waiting(since) => {
                Some(since.saturating_add(Duration::from_millis(ROUTE_WAIT_MS)))
            }
            Self::None | Self::Decided(_) => None,
        }
    }

    pub(super) fn route(self) -> Option<Route> {
        match self {
            Self::Decided(route) => Some(route),
            Self::None | Self::Waiting(_) => None,
        }
    }

    pub(super) fn fallback(&mut self) {
        if matches!(self, Self::Decided(Route::Dictation)) {
            *self = Self::Decided(Route::Uplink);
        }
    }
}

#[cfg(test)]
mod tests;
