//! Packets to state: each report the pods send lands in the fields its row names, one report per packet.

use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::model::Model;
use pods_proto::noise::NoiseMode;
use pods_proto::packet::{Incoming, classify};

use super::commands::KnownSettings;
use super::{Action, Machine, Phase};
use crate::command::AdaptiveLevel;
use crate::state::State;

impl Machine {
    /// Applies the report in one packet to `state`; a control report also answers the commands waiting for it.
    pub(super) fn report(&mut self, state: &mut State, sdu: &[u8]) -> Vec<Action> {
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
                apply_control(state, &mut self.known_settings, control);
                self.confirm(control)
            }
            Incoming::Metadata(metadata) => {
                state.model = Model::from_number(&metadata.model_number);
                state.model_number = metadata.model_number;
                state.device_name = metadata.name;
                Vec::new()
            }
            Incoming::Uplink(_) | Incoming::Other { .. } | Incoming::Foreign => Vec::new(),
        }
    }
}

/// Stores a control report's setting when a row reads its identifier and takes its value: `04 00 04 00 09 00 28 01 00 00 00` is conversation awareness on.
fn apply_control(state: &mut State, known: &mut KnownSettings, control: ControlCommand) {
    match control.id {
        ControlId::NOISE_CONTROL => {
            if let Ok(mode) = NoiseMode::from_command(&control) {
                state.noise_mode = mode;
                known.noise = true;
            }
        }
        ControlId::CONVERSATIONAL_AWARENESS => {
            if let Some(enabled) = control.state() {
                state.conversational_awareness = enabled;
                known.conversational_awareness = true;
            }
        }
        ControlId::ONE_BUD_ANC => {
            if let Some(enabled) = control.state() {
                state.one_bud_anc_mode = enabled;
                known.one_bud_anc = true;
            }
        }
        ControlId::ALLOW_OFF_OPTION => {
            if let Some(offered) = control.state() {
                state.noise_off = Some(offered);
            }
        }
        ControlId::ADAPTIVE_NOISE_LEVEL => {
            let [percent, ..] = control.value;
            if let Ok(level) = AdaptiveLevel::new(percent) {
                state.adaptive_noise_level = level;
                known.adaptive_level = true;
            }
        }
        // Every other identifier is a setting no row of the table reads.
        _ => {}
    }
}
