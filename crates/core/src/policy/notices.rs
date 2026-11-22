//! The desktop notices with their text, and the rule that shows one when the link is lost.

use crate::link::Signal;

/// No text holds the device's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    Disconnected,
}

impl Notice {
    #[must_use]
    pub fn title(self) -> String {
        match self {
            Self::Disconnected => "AirPods Disconnected".to_owned(),
        }
    }

    #[must_use]
    pub fn message(self) -> String {
        match self {
            Self::Disconnected => "Your AirPods have been disconnected".to_owned(),
        }
    }
}

/// A device reset after its link was up; one that never came up, a parked device among them, is silent.
#[must_use]
pub fn disconnected(signal: Signal) -> Option<Notice> {
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
