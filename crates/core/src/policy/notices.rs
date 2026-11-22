//! The desktop notices with their text, and the rule that shows one when the link is lost.

use crate::link::Signal;

/// No text holds the device's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    Disconnected,
    LowBattery { part: Part, percent: u8 },
}

/// A battery with a low battery notice; the headset's has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Left,
    Right,
    Case,
}

impl Part {
    fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Case => "Case",
        }
    }
}

impl Notice {
    #[must_use]
    pub fn title(self) -> String {
        match self {
            Self::Disconnected => "AirPods Disconnected".to_owned(),
            Self::LowBattery { part, .. } => format!("{} AirPod Low Battery", part.label()),
        }
    }

    #[must_use]
    pub fn message(self) -> String {
        match self {
            Self::Disconnected => "Your AirPods have been disconnected".to_owned(),
            Self::LowBattery { percent, .. } => format!("{percent}% remaining"),
        }
    }
}

/// A device reset after its link was up; one that never came up, a parked device among them, is silent.
#[must_use]
pub(super) fn disconnected(signal: Signal) -> Option<Notice> {
    match signal {
        Signal::Lost {
            populated: true, ..
        } => Some(Notice::Disconnected),
        Signal::Lost {
            populated: false, ..
        }
        | Signal::Up { .. } => None,
    }
}
