//! Support tests: no user path yet. A setting is answered from the state only once the pods reported it on the current channel.

use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::model::Model;
use pods_proto::noise::NoiseMode;

use super::reports::report;
use super::sim::{Input, Sim, ms};
use crate::command::tests::level;
use crate::command::{Command, CommandError, NoiseTarget, Outcome};
use crate::link::Phase;
use crate::link::ladder::FIRST_RETRY_MS;
use crate::state::State;

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

/// A link up with both buds in an ear and no setting reported, to AirPods Pro 3 when `pro3`, else to a model of unknown number.
fn up_unreported(pro3: bool) -> Sim {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.feed(Input::Packet(pods_samples::ear()));
    if pro3 {
        sim.feed(Input::Packet(pods_samples::metadata()));
        assert_eq!(sim.state.model, Model::AirPodsPro3);
    }
    sim
}

/// Asks `command` three times: unreported on a new channel, after the pods report `frame`, and on the next channel.
fn known_only_after_a_report_on_this_channel(
    mut sim: Sim,
    command: Command,
    frame: ControlCommand,
    shown: impl Fn(&State) -> bool,
) {
    sim.feed(Input::Command(command));
    assert_eq!(sim.last(), [written(frame)], "{command:?} unreported");
    assert_eq!(sim.outcomes(), []);
    sim.feed(Input::Packet(frame.encode()));
    assert_eq!(sim.last(), ["Resolve(0)"], "{command:?}");

    sim.feed(Input::Command(command));
    assert_eq!(sim.last(), ["Resolve(1)"], "{command:?} reported");

    reopen(&mut sim);
    assert!(shown(&sim.state), "{command:?} kept in the state");
    sim.feed(Input::Command(command));
    assert_eq!(sim.last(), [written(frame)], "{command:?} on a new channel");
    sim.feed(Input::Packet(frame.encode()));
    assert_eq!(sim.last(), ["Resolve(2)"], "{command:?}");
    assert_eq!(sim.outcomes(), [Outcome::Confirmed; 3]);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_noise_mode_is_written_until_the_pods_report_it_on_this_channel() {
    assert_eq!(State::default().noise_mode, NoiseMode::Off);
    for mode in [NoiseMode::Off, NoiseMode::Anc] {
        known_only_after_a_report_on_this_channel(
            up_unreported(false),
            Command::SetNoise(NoiseTarget::Mode(mode)),
            mode.command(),
            |state| state.noise_mode == mode,
        );
    }
}

#[test]
fn conversational_awareness_is_written_until_the_pods_report_it_on_this_channel() {
    assert!(!State::default().conversational_awareness);
    for enabled in [false, true] {
        known_only_after_a_report_on_this_channel(
            up_unreported(true),
            Command::SetConversationalAwareness(enabled),
            ControlCommand::switch(ControlId::CONVERSATIONAL_AWARENESS, enabled),
            |state| state.conversational_awareness == enabled,
        );
    }
}

#[test]
fn one_bud_anc_is_written_until_the_pods_report_it_on_this_channel() {
    assert!(!State::default().one_bud_anc_mode);
    for enabled in [false, true] {
        known_only_after_a_report_on_this_channel(
            up_unreported(true),
            Command::SetOneBudAnc(enabled),
            ControlCommand::switch(ControlId::ONE_BUD_ANC, enabled),
            |state| state.one_bud_anc_mode == enabled,
        );
    }
}

#[test]
fn an_adaptive_level_is_written_unless_stored_or_reported_on_this_channel() {
    let adaptive = NoiseMode::Adaptive.command().encode();
    let level_frame = |level: u8| {
        written(ControlCommand {
            id: ControlId::ADAPTIVE_NOISE_LEVEL,
            value: [level, 0, 0, 0],
        })
    };
    let mut sim = up_unreported(true);
    sim.feed(Input::Packet(adaptive.clone()));
    let default = sim.state.adaptive_noise_level;
    sim.feed(Input::Command(Command::SetAdaptiveLevel(default)));
    assert_eq!(
        sim.since(2),
        [level_frame(default.percent()), "Resolve(0)".to_owned()]
    );

    sim.feed(Input::Command(Command::SetAdaptiveLevel(level(30))));
    assert_eq!(sim.since(2), [level_frame(30), "Resolve(1)".to_owned()]);
    sim.feed(Input::Command(Command::SetAdaptiveLevel(level(30))));
    assert_eq!(sim.last(), ["Resolve(2)"]);

    reopen(&mut sim);
    sim.feed(Input::Packet(adaptive.clone()));
    assert_eq!(sim.state.adaptive_noise_level, level(30));
    sim.feed(Input::Command(Command::SetAdaptiveLevel(level(30))));
    assert_eq!(sim.since(2), [level_frame(30), "Resolve(3)".to_owned()]);

    reopen(&mut sim);
    sim.feed(Input::Packet(adaptive));
    sim.feed(Input::Packet(report(ControlId::ADAPTIVE_NOISE_LEVEL, 40)));
    sim.feed(Input::Command(Command::SetAdaptiveLevel(level(40))));
    assert_eq!(sim.last(), ["Resolve(4)"]);
    assert_eq!(
        sim.outcomes(),
        [
            Outcome::Sent,
            Outcome::Sent,
            Outcome::Confirmed,
            Outcome::Sent,
            Outcome::Confirmed
        ]
    );
    sim.check_all_tickets_resolved_once();
}

#[test]
fn an_adaptive_level_is_refused_until_this_channel_reports_adaptive() {
    let mut sim = up_unreported(true);
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
