//! The C++ daemon's low battery latch, one for each bud and one for the case.

use pods_proto::battery::{Batteries, Charge};

use super::notices::{Notice, Part};

/// C++ `LowBatteryLatch` trigger.
pub(super) const LOW_BATTERY_TRIGGER: u8 = 10;

/// C++ latch reset; five points of hysteresis.
pub(super) const LOW_BATTERY_RESET: u8 = 15;

#[derive(Debug, Default)]
pub(super) struct Latch {
    notified: bool,
}

impl Latch {
    /// The percent to announce, exactly when a notice is due, as the C++ `evaluate` returns it; no reading changes nothing.
    pub(super) fn evaluate(&mut self, charge: Option<Charge>) -> Option<u8> {
        let Charge { level, charging } = charge?;
        if charging {
            self.notified = false;
            return None;
        }
        if level <= LOW_BATTERY_TRIGGER && !self.notified {
            self.notified = true;
            return Some(level);
        }
        if level >= LOW_BATTERY_RESET {
            self.notified = false;
        }
        None
    }
}

/// The headset's battery has none, as in the C++ daemon.
#[derive(Debug, Default)]
pub(super) struct Latches {
    left: Latch,
    right: Latch,
    case: Latch,
}

impl Latches {
    /// The notices due, left, right then case, the order the C++ walks them in.
    pub(super) fn due(&mut self, batteries: Batteries) -> Vec<Notice> {
        [
            (Part::Left, &mut self.left, batteries.left),
            (Part::Right, &mut self.right, batteries.right),
            (Part::Case, &mut self.case, batteries.case),
        ]
        .into_iter()
        .filter_map(|(part, latch, charge)| {
            latch
                .evaluate(charge)
                .map(|percent| Notice::LowBattery { part, percent })
        })
        .collect()
    }
}
