//! Support tests: what one channel holds of the settings ends when the next channel opens.

use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::model::Model;
use pods_proto::noise::NoiseMode;

use super::sim::{Input, Sim, ms};
use crate::command::tests::level;
use crate::command::{Command, CommandError, NoiseTarget, Outcome};
use crate::link::Phase;
use crate::link::ladder::FIRST_RETRY_MS;

/// The frame of an adaptive level of `percent`.
fn adaptive_level(percent: u8) -> ControlCommand {
    ControlCommand {
        id: ControlId::ADAPTIVE_NOISE_LEVEL,
        value: [percent, 0, 0, 0],
    }
}

/// The step's action for a frame written to the pods.
fn written(frame: ControlCommand) -> String {
    format!("Send({:02x?})", frame.encode())
}

/// The peer closes the channel and the first retry opens a new one, with no setting reported on it.
fn reopen(sim: &mut Sim) {
    sim.feed(Input::PeerCloses);
    sim.feed(Input::Wait(ms(FIRST_RETRY_MS)));
    sim.feed(Input::Packet(pods_samples::handshake_ack()));
    sim.feed(Input::Packet(pods_samples::features_ack()));
    assert!(matches!(sim.machine.phase(), Phase::Up { .. }));
}

/// A link up to AirPods Pro 3 with both buds in an ear and no setting reported.
fn up_unreported() -> Sim {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.feed(Input::Packet(pods_samples::ear()));
    sim.feed(Input::Packet(pods_samples::metadata()));
    assert_eq!(sim.state.model, Model::AirPodsPro3);
    sim
}

#[test]
fn a_new_channel_takes_its_opening_reports_whatever_was_written_before() {
    let mut sim = up_unreported();
    sim.feed(Input::Command(Command::SetNoise(NoiseTarget::Mode(
        NoiseMode::Anc,
    ))));
    let transparency = NoiseMode::Transparency.command().encode();
    sim.feed(Input::Packet(transparency.clone()));
    assert_eq!(
        sim.state.noise_mode,
        NoiseMode::Anc,
        "held on the same channel"
    );
    reopen(&mut sim);
    sim.feed(Input::Packet(transparency));
    assert_eq!(sim.state.noise_mode, NoiseMode::Transparency);
    assert_eq!(sim.outcomes(), [Outcome::Sent]);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn an_adaptive_level_is_written_once_this_channel_wrote_adaptive() {
    let mut sim = up_unreported();
    sim.feed(Input::Command(Command::SetNoise(NoiseTarget::Mode(
        NoiseMode::Adaptive,
    ))));
    sim.feed(Input::Command(Command::SetAdaptiveLevel(level(30))));
    assert_eq!(sim.since(2)[0], written(adaptive_level(30)));
    assert_eq!(sim.outcomes(), [Outcome::Sent; 2]);
    assert_eq!(sim.state.adaptive_noise_level, level(30));
    sim.check_all_tickets_resolved_once();
}

#[test]
fn an_adaptive_level_is_refused_on_a_new_channel_until_it_reports_or_writes_adaptive() {
    let mut sim = up_unreported();
    sim.feed(Input::Packet(NoiseMode::Adaptive.command().encode()));
    reopen(&mut sim);
    assert_eq!(sim.state.noise_mode, NoiseMode::Adaptive);
    sim.feed(Input::Command(Command::SetAdaptiveLevel(level(30))));
    assert_eq!(sim.last(), ["Resolve(0)"]);
    assert_eq!(
        sim.outcomes(),
        [Outcome::Refused(CommandError::NotSupported)]
    );
    sim.check_all_tickets_resolved_once();
}
