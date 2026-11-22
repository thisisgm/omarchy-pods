//! The desktop's audio server: which device's sink the adapter follows, what it reports, and what `audio_server` reads.

use pods_proto::addr::BdAddr;

use super::Fact;
use crate::link::Signal;
use crate::state::AudioServer;

/// A volume as the desktop shows it, 0 to 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Percent(u8);

impl Percent {
    /// The top of the scale; a boosted sink reads as this.
    pub const FULL: Self = Self(100);

    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value <= Self::FULL.0 {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// What the adapter learned of the audio server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFact {
    Up,
    Down,
    /// `None`: the followed device has no sink.
    Sink(Option<SinkFact>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkFact {
    pub is_default: bool,
    pub volume: Percent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRequest {
    /// `None` closes the connection.
    Follow(Option<BdAddr>),
}

/// The connection follows a device from its link coming up to the device's reset, so a channel retry keeps it.
pub(super) fn follow(signal: Signal) -> AudioRequest {
    match signal {
        Signal::Up { device } => AudioRequest::Follow(Some(device)),
        Signal::Lost { .. } => AudioRequest::Follow(None),
    }
}

/// `down` from the link's up until the adapter reports the server, `idle` from its loss, when a late report is dropped.
#[must_use]
pub fn server_after(was: AudioServer, fact: Fact) -> AudioServer {
    match fact {
        Fact::Link(Signal::Up { .. }) => AudioServer::Down,
        Fact::Link(Signal::Lost { .. }) => AudioServer::Idle,
        Fact::Audio(AudioFact::Up) if was != AudioServer::Idle => AudioServer::Up,
        Fact::Audio(AudioFact::Down) if was != AudioServer::Idle => AudioServer::Down,
        Fact::Audio(_) | Fact::Changed | Fact::Sleep(_) | Fact::Tick => was,
    }
}

#[cfg(test)]
mod tests;
