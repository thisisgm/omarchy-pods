//! Packets to state: each report the pods send lands in the fields its row names, one report per packet.

use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::model::Model;
use pods_proto::packet::{Incoming, classify};
use pods_proto::packets::REQUEST_PROXIMITY_KEYS;

use std::time::Duration;

use super::commands::Setting;
use super::{Action, Machine, Phase, Signal};
use crate::state::State;

impl Machine {
    /// Applies the report in one packet to `state`; a control report held back by an earlier write is dropped.
    pub(super) fn report(&mut self, state: &mut State, sdu: &[u8], now: Duration) -> Vec<Action> {
        // A packet no parser here reads, or one its parser refuses, changes nothing and is not logged.
        let Ok(incoming) = classify(sdu) else {
            return Vec::new();
        };
        match incoming {
            Incoming::Battery(report) => {
                state.batteries.apply(&report);
                // corner: a report that names no primary bud keeps the last one, as `Batteries::apply` keeps a missing part.
                if let Some(primary) = report.primary {
                    state.primary = Some(primary);
                }
                self.battery_seen = true;
                if let Phase::Up { rerequest: Some(_) } = self.phase {
                    self.enter(state, Phase::Up { rerequest: None });
                }
                Vec::new()
            }
            Incoming::Ear(report) => {
                state.ear = report;
                Vec::new()
            }
            Incoming::Control(control) => {
                let settings = &mut self.channel_settings;
                match Setting::read(control) {
                    Some(setting) if settings.admit(setting, now) => settings.show(state, setting),
                    Some(_) => {}
                    None => apply_unheld(state, control),
                }
                Vec::new()
            }
            Incoming::Metadata(metadata) => {
                state.model = Model::from_number(&metadata.model_number);
                state.model_number = metadata.model_number;
                state.device_name = metadata.name;
                // As the C++ daemon asks, with no wait and no retry: pods that never answer cost nothing.
                vec![Action::Send(REQUEST_PROXIMITY_KEYS.to_vec())]
            }
            Incoming::Keys(keys) => vec![Action::Signal(Signal::Keys(keys))],
            Incoming::Uplink(_) | Incoming::Other { .. } | Incoming::Foreign => Vec::new(),
        }
    }
}

impl Machine {
    /// Takes every report a passed hold held back, so the state ends on the pods' last report.
    pub(super) fn release_held(&mut self, state: &mut State, now: Duration) {
        let settings = &mut self.channel_settings;
        for setting in settings.release(now) {
            settings.show(state, setting);
        }
    }
}

/// Stores a valid report of a setting no write holds: `04 00 04 00 09 00 34 01 00 00 00` offers Off.
fn apply_unheld(state: &mut State, control: ControlCommand) {
    // A held identifier with a value out of range lands here and changes nothing.
    if control.id == ControlId::ALLOW_OFF_OPTION
        && let Some(offered) = control.state()
    {
        state.noise_off = Some(offered);
    }
}
