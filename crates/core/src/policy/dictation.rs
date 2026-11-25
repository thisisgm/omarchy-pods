//! The dictation service as the policy and the adapter of the pods' attribute channel speak of it: what is asked of the channel, and what it tells back.

use pods_proto::dictation::Event;
use pods_proto::doap::ArmFailure;

/// What the policy asks for; the driver writes `Mode` on the control channel and hands the rest to the attribute channel's adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationRequest {
    /// The setting that lets a start stream.
    Mode,
    /// Open the attribute channel and subscribe to the service, over any channel still open; told back as `Armed`, `ArmFailed` or `BearerFailed`.
    Arm,
    /// Sent only on an armed channel.
    Start,
    /// Sent only on an armed channel.
    Stop { reason: u8 },
    /// Drop the attribute channel; nothing is told back.
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictationFact {
    Armed,
    /// The table or the pods refused the arm, and the channel was dropped.
    ArmFailed(ArmFailure),
    /// The channel did not open, or it ended, went silent or took no write before the arm was done.
    BearerFailed,
    /// The armed channel ended or took no write; told once.
    BearerLost,
    /// The pods answered a start the host asked for with the started score (`Event::STARTED`).
    Answered,
    /// The pods' stop notification; reason 6 refuses a start.
    Refused {
        reason: u8,
    },
    /// The stream ended, after a stop or by the pods.
    Ended,
    /// Any other event of the service.
    PodEvent(Event),
    /// The first audio since the last start went out.
    FirstAudio,
    /// The audio notifications read since the daemon started, at every tick.
    Heard {
        packets: u64,
    },
}
