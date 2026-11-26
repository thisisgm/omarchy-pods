//! Wear at control link loss, used to decide whether to reconnect.

use pods_proto::addr::BdAddr;
use pods_proto::ear::EarReport;

use super::{Machine, Phase};
use crate::state::State;

impl Machine {
    /// Keeps wear at link loss while BlueZ still reports the device connected.
    pub(super) fn record_wear_at_link_down(&mut self, was_up: bool, down: bool, ear: EarReport) {
        if !self.held || !down {
            self.ear_at_link_down = EarReport::default();
        } else if was_up {
            self.ear_at_link_down = ear;
        }
    }

    /// Checks wear at the last live control link before processing a BlueZ device disconnect.
    #[must_use]
    pub fn unrequested_down_while_worn(&self, device: BdAddr, state: &State) -> bool {
        let ear = if matches!(self.phase, Phase::Up { .. }) {
            state.ear
        } else {
            self.ear_at_link_down
        };
        self.is_active(device) && self.user_disconnects == 0 && ear.any_in_ear()
    }
}
