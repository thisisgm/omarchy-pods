//! The desktop's audio server: which device's sink the adapter follows, what it reports, and what `audio_server` reads.

use pods_proto::addr::BdAddr;

use super::Fact;
use crate::link::Signal;
use crate::state::AudioServer;

/// How `PipeWire` names a Bluetooth device's output, before the address.
const SINK_PREFIX: &str = "bluez_output.";

/// `bluez_output.` then the address in capitals with underscores, as `bluez_output.02_00_00_00_00_01`.
#[must_use]
pub fn sink_prefix(device: BdAddr) -> String {
    let groups: Vec<String> = device
        .octets()
        .iter()
        .map(|octet| format!("{octet:02X}"))
        .collect();
    format!("{SINK_PREFIX}{}", groups.join("_"))
}

/// A judgment: a server slower than this is treated as gone.
pub const AUDIO_CALL_TIMEOUT_MS: u64 = 2000;

/// The measured form of the pause between a sink's suspend and its resume.
pub const RESTORE_HOLD_MS: u64 = 300;

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
    /// The restore asked for under this id is done, or kept owed while the server is down.
    Restored(RestoreId),
}

/// Which restore an answer is for, so a late answer to an earlier one finishes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreId(pub(super) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkFact {
    pub is_default: bool,
    pub volume: Percent,
    pub state: SinkState,
}

/// Only `Suspended`, or no sink at all, lets a STOP out while a bud is worn; an idle sink can still hold a started stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkState {
    Running,
    Idle,
    Suspended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRequest {
    /// `None` closes the connection.
    Follow(Option<BdAddr>),
    /// Suspends and resumes the followed device's sink, so a stream the uplink's end broke is opened again.
    Restore(RestoreId),
    /// A START went out, so a restore still owed is dropped: the newest intent wins.
    CancelRestore,
}

/// The connection follows a device from its link coming up to the device's reset, so a channel retry keeps it.
pub(super) fn follow(signal: &Signal) -> Option<AudioRequest> {
    match signal {
        Signal::Up { device } => Some(AudioRequest::Follow(Some(*device))),
        Signal::Lost { .. } => Some(AudioRequest::Follow(None)),
        Signal::Down | Signal::Keys(_) | Signal::Uplink(_) => None,
    }
}

/// `down` from the link's up until the adapter reports the server, `idle` from its loss, when a late report is dropped.
#[must_use]
pub fn server_after(was: AudioServer, fact: &Fact) -> AudioServer {
    match fact {
        Fact::Link(Signal::Up { .. }) => AudioServer::Down,
        Fact::Link(Signal::Lost { .. }) => AudioServer::Idle,
        Fact::Audio(AudioFact::Up) if was != AudioServer::Idle => AudioServer::Up,
        Fact::Audio(AudioFact::Down) if was != AudioServer::Idle => AudioServer::Down,
        Fact::Link(Signal::Down | Signal::Keys(_) | Signal::Uplink(_))
        | Fact::Audio(_)
        | Fact::Advert(_)
        | Fact::Monitor(_)
        | Fact::Changed
        | Fact::Sleep(_)
        | Fact::Tick
        | Fact::Paused(..)
        | Fact::Mic(_)
        | Fact::Exit => was,
    }
}

#[cfg(test)]
mod tests;
