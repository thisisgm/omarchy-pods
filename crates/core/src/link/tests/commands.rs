//! Commands to bytes: one test per row and column of the table, and every way a waiting command ends.

use pods_proto::control::ControlId;
use pods_proto::model::Model;
use pods_proto::noise::NoiseMode;

use super::reports::report;
use super::sim::{Answer, Input, Sim, ms};
use crate::command::{self, Command, CommandError, NoiseTarget, Outcome};
use crate::link::Phase;
use crate::link::ladder::PENDING_MAX;
use crate::state::{Counters, EarDetectionBehavior};

const NO_ACTIONS: [&str; 0] = [];
const OFF_FRAME: &str = "Send([04, 00, 04, 00, 09, 00, 0d, 01, 00, 00, 00])";
const ANC_FRAME: &str = "Send([04, 00, 04, 00, 09, 00, 0d, 02, 00, 00, 00])";
const TRANSPARENCY_FRAME: &str = "Send([04, 00, 04, 00, 09, 00, 0d, 03, 00, 00, 00])";
const ADAPTIVE_FRAME: &str = "Send([04, 00, 04, 00, 09, 00, 0d, 04, 00, 00, 00])";
const TIMEOUT: Outcome = Outcome::Refused(CommandError::Timeout);
const NOT_SUPPORTED: Outcome = Outcome::Refused(CommandError::NotSupported);
const NOT_CONNECTED: Outcome = Outcome::Refused(CommandError::NotConnected);

/// Both buds out of the ear: header 04 00 04 00 06 00, then primary 01 and secondary 01.
const OUT_OF_EAR: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x01, 0x01];
/// Both buds in the case.
const IN_CASE: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x02, 0x02];

/// Every command a client can send, with levels on both sides of the bound.
const EVERY_COMMAND: [Command; 18] = [
    Command::SetNoise(NoiseTarget::Mode(NoiseMode::Off)),
    Command::SetNoise(NoiseTarget::Mode(NoiseMode::Anc)),
    Command::SetNoise(NoiseTarget::Mode(NoiseMode::Transparency)),
    Command::SetNoise(NoiseTarget::Mode(NoiseMode::Adaptive)),
    Command::SetNoise(NoiseTarget::Cycle),
    Command::SetEarDetection(EarDetectionBehavior::PauseWhenOneRemoved),
    Command::SetEarDetection(EarDetectionBehavior::PauseWhenBothRemoved),
    Command::SetEarDetection(EarDetectionBehavior::Disabled),
    Command::SetConversationalAwareness(true),
    Command::SetConversationalAwareness(false),
    Command::SetOneBudAnc(true),
    Command::SetOneBudAnc(false),
    Command::SetAdaptiveLevel(30),
    Command::SetAdaptiveLevel(u8::MAX),
    Command::Connect,
    Command::Disconnect,
    Command::Forget,
    Command::Reopen,
];

fn noise(mode: NoiseMode) -> Command {
    Command::SetNoise(NoiseTarget::Mode(mode))
}

/// A link up to a device of unknown model (noise modes Off, Anc and Transparency), both buds in an ear, mode Off.
fn up_unknown() -> Sim {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.feed(Input::Packet(pods_samples::ear()));
    sim
}

/// A link up to AirPods Pro 3 (no Off), both buds in an ear, in `mode`.
fn up_pro3(mode: NoiseMode) -> Sim {
    let mut sim = up_unknown();
    sim.feed(Input::Packet(pods_samples::metadata()));
    sim.feed(Input::Packet(mode.command().encode()));
    assert_eq!(sim.state.model, Model::AirPodsPro3);
    assert_eq!(sim.state.noise_mode, mode);
    sim
}

#[test]
fn noise_anc_writes_the_frame_and_is_confirmed_by_the_report() {
    let mut sim = up_unknown();
    sim.feed(Input::Command(noise(NoiseMode::Anc)));
    assert_eq!(sim.last(), [ANC_FRAME]);
    assert_eq!(sim.outcomes(), []);
    assert_eq!(sim.state.noise_mode, NoiseMode::Off);
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x03)));
    assert_eq!(sim.last(), NO_ACTIONS);
    assert_eq!(sim.state.noise_mode, NoiseMode::Transparency);
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x02)));
    assert_eq!(sim.last(), ["Resolve(0)"]);
    assert_eq!(sim.outcomes(), [Outcome::Confirmed]);
    assert_eq!(sim.state.noise_mode, NoiseMode::Anc);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_mode_already_set_is_confirmed_without_bytes() {
    let mut sim = up_unknown();
    let counters = sim.state.counters;
    sim.feed(Input::Command(noise(NoiseMode::Off)));
    assert_eq!(sim.last(), ["Resolve(0)"]);
    let mut sim3 = up_pro3(NoiseMode::Adaptive);
    sim3.feed(Input::Command(noise(NoiseMode::Adaptive)));
    assert_eq!(sim3.last(), ["Resolve(0)"]);
    assert_eq!(sim.outcomes(), [Outcome::Confirmed]);
    assert_eq!(sim3.outcomes(), [Outcome::Confirmed]);
    assert_eq!(sim.state.counters, counters);
    sim.check_all_tickets_resolved_once();
    sim3.check_all_tickets_resolved_once();
}

#[test]
fn off_on_a_model_without_off_is_not_supported() {
    let mut sim = up_pro3(NoiseMode::Anc);
    sim.feed(Input::Command(noise(NoiseMode::Off)));
    assert_eq!(sim.last(), ["Resolve(0)"]);
    assert_eq!(sim.outcomes(), [NOT_SUPPORTED]);
    assert_eq!(sim.state.noise_mode, NoiseMode::Anc);
    assert_eq!(sim.state.counters.noise_control_changes, 0);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn adaptive_on_a_model_without_it_and_noise_on_a_model_without_noise_control_are_not_supported() {
    let mut sim = up_unknown();
    sim.feed(Input::Command(noise(NoiseMode::Adaptive)));
    assert_eq!(sim.last(), ["Resolve(0)"]);
    sim.state.model = Model::AirPods4;
    for command in [
        noise(NoiseMode::Off),
        noise(NoiseMode::Anc),
        noise(NoiseMode::Transparency),
        Command::SetNoise(NoiseTarget::Cycle),
    ] {
        sim.feed(Input::Command(command));
        assert_eq!(sim.last().len(), 1, "{command:?}");
    }
    assert_eq!(sim.outcomes(), [NOT_SUPPORTED; 5]);
    assert_eq!(sim.state.counters.noise_control_changes, 0);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn noise_set_without_a_report_is_timeout_after_700_ms() {
    let mut sim = up_unknown();
    sim.run_until(ms(100));
    sim.feed(Input::Command(noise(NoiseMode::Anc)));
    assert_eq!(sim.machine.deadline(), Some(ms(800)));
    sim.run_until(ms(799));
    assert_eq!(sim.outcomes(), []);
    sim.run_until(ms(800));
    assert_eq!(sim.times_of("Resolve(0)"), [800]);
    assert_eq!(sim.outcomes(), [TIMEOUT]);
    assert_eq!(sim.state.noise_mode, NoiseMode::Off);
    // A late report still lands in the state, and answers nothing.
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x02)));
    assert_eq!(sim.last(), NO_ACTIONS);
    assert_eq!(sim.state.noise_mode, NoiseMode::Anc);

    sim.feed(Input::Command(Command::SetNoise(NoiseTarget::Cycle)));
    assert_eq!(sim.last(), [TRANSPARENCY_FRAME]);
    sim.run_until(ms(1500));
    assert_eq!(sim.times_of("Resolve(1)"), [1500]);
    assert_eq!(sim.outcomes(), [TIMEOUT, TIMEOUT]);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn noise_set_without_a_report_is_not_worn_when_no_bud_is_in_an_ear() {
    let not_worn = Outcome::Refused(CommandError::NotWorn);
    for placement in [OUT_OF_EAR, IN_CASE] {
        let mut sim = up_unknown();
        sim.feed(Input::Packet(placement.to_vec()));
        sim.feed(Input::Command(noise(NoiseMode::Anc)));
        sim.feed(Input::Command(Command::SetNoise(NoiseTarget::Cycle)));
        sim.run_until(ms(700));
        assert_eq!(sim.outcomes(), [not_worn, not_worn]);
        sim.check_all_tickets_resolved_once();
    }

    // A headset has no ear placement to ask about, so it times out.
    let mut sim = up_unknown();
    sim.feed(Input::Packet(OUT_OF_EAR.to_vec()));
    sim.state.model = Model::AirPodsMaxUSBC;
    sim.feed(Input::Command(noise(NoiseMode::Anc)));
    sim.run_until(ms(700));
    assert_eq!(sim.outcomes(), [TIMEOUT]);
    sim.check_all_tickets_resolved_once();

    // The placement at the deadline decides, not the one at the send.
    let mut sim = up_unknown();
    sim.feed(Input::Command(noise(NoiseMode::Anc)));
    sim.feed(Input::Packet(OUT_OF_EAR.to_vec()));
    sim.run_until(ms(700));
    assert_eq!(sim.outcomes(), [not_worn]);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn cycle_skips_modes_the_model_lacks() {
    let cycle = Command::SetNoise(NoiseTarget::Cycle);
    for (mode, frame) in [
        (NoiseMode::Adaptive, ANC_FRAME),
        (NoiseMode::Anc, TRANSPARENCY_FRAME),
        (NoiseMode::Transparency, ADAPTIVE_FRAME),
    ] {
        let mut sim = up_pro3(mode);
        sim.feed(Input::Command(cycle));
        assert_eq!(sim.last(), [frame], "Pro 3 from {mode:?}");
        sim.check();
    }
    for (mode, frame) in [
        (NoiseMode::Off, ANC_FRAME),
        (NoiseMode::Anc, TRANSPARENCY_FRAME),
        (NoiseMode::Transparency, OFF_FRAME),
    ] {
        let mut sim = up_unknown();
        sim.feed(Input::Packet(mode.command().encode()));
        sim.feed(Input::Command(cycle));
        assert_eq!(sim.last(), [frame], "unknown model from {mode:?}");
        sim.check();
    }
    // A model with both Off and Adaptive goes from Adaptive to Off.
    let mut sim = up_pro3(NoiseMode::Adaptive);
    sim.state.model = Model::AirPodsPro2USBC;
    sim.feed(Input::Command(cycle));
    assert_eq!(sim.last(), [OFF_FRAME]);
    sim.check();

    let mut sim = up_pro3(NoiseMode::Adaptive);
    sim.feed(Input::Command(cycle));
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x02)));
    assert_eq!(sim.outcomes(), [Outcome::Confirmed]);
    assert_eq!(sim.state.noise_mode, NoiseMode::Anc);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn conversational_awareness_and_one_bud_follow_the_same_rule() {
    let mut sim = up_pro3(NoiseMode::Anc);
    sim.feed(Input::Command(Command::SetConversationalAwareness(true)));
    assert_eq!(
        sim.last(),
        ["Send([04, 00, 04, 00, 09, 00, 28, 01, 00, 00, 00])"]
    );
    assert!(!sim.state.conversational_awareness);
    sim.feed(Input::Packet(report(
        ControlId::CONVERSATIONAL_AWARENESS,
        0x02,
    )));
    assert_eq!(sim.last(), NO_ACTIONS);
    // The same value under another identifier is not this setting's report.
    sim.feed(Input::Packet(report(ControlId::MIC_SIDE, 0x01)));
    assert_eq!(sim.last(), NO_ACTIONS);
    sim.feed(Input::Packet(pods_samples::control()));
    assert_eq!(sim.last(), ["Resolve(0)"]);
    assert!(sim.state.conversational_awareness);
    sim.feed(Input::Command(Command::SetConversationalAwareness(true)));
    assert_eq!(sim.last(), ["Resolve(1)"]);

    sim.feed(Input::Command(Command::SetOneBudAnc(false)));
    assert_eq!(sim.last(), ["Resolve(2)"]);
    sim.feed(Input::Command(Command::SetOneBudAnc(true)));
    assert_eq!(
        sim.last(),
        ["Send([04, 00, 04, 00, 09, 00, 1b, 01, 00, 00, 00])"]
    );
    sim.feed(Input::Command(Command::SetConversationalAwareness(false)));
    assert_eq!(
        sim.last(),
        ["Send([04, 00, 04, 00, 09, 00, 28, 02, 00, 00, 00])"]
    );
    // Only a noise change asks where the buds are; these two time out.
    sim.feed(Input::Packet(OUT_OF_EAR.to_vec()));
    sim.run_until(sim.now.saturating_add(ms(700)));
    assert_eq!(
        sim.outcomes(),
        [
            Outcome::Confirmed,
            Outcome::Confirmed,
            Outcome::Confirmed,
            TIMEOUT,
            TIMEOUT
        ]
    );
    assert!(!sim.state.one_bud_anc_mode);
    assert!(sim.state.conversational_awareness);
    sim.check_all_tickets_resolved_once();

    let mut sim = up_unknown();
    for command in [
        Command::SetConversationalAwareness(true),
        Command::SetConversationalAwareness(false),
        Command::SetOneBudAnc(true),
        Command::SetOneBudAnc(false),
    ] {
        sim.feed(Input::Command(command));
        assert_eq!(sim.last().len(), 1, "{command:?}");
    }
    assert_eq!(sim.outcomes(), [NOT_SUPPORTED; 4]);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn adaptive_level_is_sent_and_stored_at_once() {
    let mut sim = up_pro3(NoiseMode::Adaptive);
    sim.feed(Input::Command(Command::SetAdaptiveLevel(30)));
    assert_eq!(
        sim.last(),
        [
            "Send([04, 00, 04, 00, 09, 00, 2e, 1e, 00, 00, 00])",
            "Resolve(0)"
        ]
    );
    assert_eq!(sim.outcomes(), [Outcome::Sent]);
    assert_eq!(sim.state.adaptive_noise_level, 30);
    assert_eq!(sim.state.counters.adaptive_level_changes, 1);
    assert_eq!(sim.machine.pending.len(), 0);

    sim.feed(Input::Command(Command::SetAdaptiveLevel(30)));
    assert_eq!(sim.last(), ["Resolve(1)"]);
    assert_eq!(sim.outcome(1), [Outcome::Confirmed]);
    assert_eq!(sim.state.counters.adaptive_level_changes, 1);

    sim.feed(Input::Command(Command::SetAdaptiveLevel(u8::MAX)));
    assert_eq!(
        sim.last(),
        [
            "Send([04, 00, 04, 00, 09, 00, 2e, 64, 00, 00, 00])",
            "Resolve(2)"
        ]
    );
    assert_eq!(sim.state.adaptive_noise_level, 100);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn adaptive_level_outside_adaptive_mode_is_not_supported() {
    let mut sim = up_pro3(NoiseMode::Anc);
    let level = sim.state.adaptive_noise_level;
    sim.feed(Input::Command(Command::SetAdaptiveLevel(30)));
    assert_eq!(sim.last(), ["Resolve(0)"]);

    let mut unknown = up_unknown();
    unknown.feed(Input::Packet(NoiseMode::Adaptive.command().encode()));
    unknown.feed(Input::Command(Command::SetAdaptiveLevel(30)));
    assert_eq!(unknown.last(), ["Resolve(0)"]);

    assert_eq!(sim.outcomes(), [NOT_SUPPORTED]);
    assert_eq!(unknown.outcomes(), [NOT_SUPPORTED]);
    assert_eq!(sim.state.adaptive_noise_level, level);
    assert_eq!(unknown.state.adaptive_noise_level, level);
    assert_eq!(sim.state.counters.adaptive_level_changes, 0);
    sim.check_all_tickets_resolved_once();
    unknown.check_all_tickets_resolved_once();
}

#[test]
fn two_commands_for_one_setting_each_get_their_own_answer() {
    let mut sim = up_unknown();
    sim.feed(Input::Command(noise(NoiseMode::Anc)));
    sim.feed(Input::Command(noise(NoiseMode::Transparency)));
    sim.feed(Input::Command(noise(NoiseMode::Anc)));
    assert_eq!(sim.times_of(ANC_FRAME), [0, 0]);
    assert_eq!(sim.times_of(TRANSPARENCY_FRAME), [0]);
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x03)));
    assert_eq!(sim.last(), ["Resolve(1)"]);
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x02)));
    assert_eq!(sim.last(), ["Resolve(0)", "Resolve(2)"]);
    assert_eq!(sim.outcomes(), [Outcome::Confirmed; 3]);
    // With nothing pending the deadline is the notification re-request again.
    assert_eq!(sim.machine.deadline(), Some(ms(2000)));
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_seventeenth_pending_command_is_refused_with_timeout() {
    let mut sim = up_unknown();
    for _ in 0..PENDING_MAX {
        sim.feed(Input::Command(noise(NoiseMode::Anc)));
        assert_eq!(sim.last(), [ANC_FRAME]);
    }
    assert_eq!(sim.state.counters.noise_control_changes, 16);
    sim.feed(Input::Command(noise(NoiseMode::Transparency)));
    assert_eq!(sim.last(), ["Resolve(16)"]);
    assert_eq!(sim.outcome(16), [TIMEOUT]);
    assert_eq!(sim.state.counters.noise_control_changes, 16);
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x02)));
    assert_eq!(sim.last().len(), PENDING_MAX);
    sim.feed(Input::Command(noise(NoiseMode::Transparency)));
    assert_eq!(sim.last(), [TRANSPARENCY_FRAME]);
    assert!((0..16).all(|ticket| sim.outcome(ticket) == [Outcome::Confirmed]));
    sim.check();
}

/// A link up with three commands waiting for their reports.
fn three_pending() -> Sim {
    let mut sim = up_unknown();
    for mode in [NoiseMode::Anc, NoiseMode::Transparency, NoiseMode::Anc] {
        sim.feed(Input::Command(noise(mode)));
    }
    assert_eq!(sim.machine.pending.len(), 3);
    sim
}

const THREE_REFUSED: [&str; 3] = ["Resolve(0)", "Resolve(1)", "Resolve(2)"];

#[test]
fn a_pending_set_resolves_when_the_channel_closes() {
    let mut sim = three_pending();
    sim.feed(Input::PeerCloses);
    assert_eq!(sim.step_of("Ended(Closed)").actions, THREE_REFUSED);
    assert_eq!(sim.outcomes(), [NOT_CONNECTED; 3]);
    assert!(matches!(sim.machine.phase(), Phase::Backoff { .. }));
    sim.check_all_tickets_resolved_once();
}

#[test]
fn every_way_out_of_up_resolves_the_pending_commands_in_that_step() {
    let mut sim = three_pending();
    sim.feed(Input::Down(0));
    assert_eq!(
        sim.step_of("Down(0)").actions,
        ["Close", "Resolve(0)", "Resolve(1)", "Resolve(2)"]
    );
    assert_eq!(sim.step_of("Ended(Closed)").actions, NO_ACTIONS);
    assert_eq!(sim.outcomes(), [NOT_CONNECTED; 3]);
    sim.check_all_tickets_resolved_once();

    let mut sim = three_pending();
    sim.feed(Input::Gone(0));
    assert_eq!(
        sim.step_of("Gone(0)").actions,
        ["Close", "Sweep", "Resolve(0)", "Resolve(1)", "Resolve(2)"]
    );
    assert_eq!(sim.outcomes(), [NOT_CONNECTED; 3]);
    sim.check_all_tickets_resolved_once();

    let mut sim = three_pending();
    sim.feed(Input::Shutdown);
    assert_eq!(
        sim.step_of("Shutdown").actions,
        ["Close", "Resolve(0)", "Resolve(1)", "Resolve(2)"]
    );
    assert_eq!(sim.outcomes(), [NOT_CONNECTED; 3]);
    sim.check_all_tickets_resolved_once();

    // A lost BlueZ keeps the link, so the commands keep waiting for their reports.
    let mut sim = three_pending();
    sim.feed(Input::BluezLost);
    assert_eq!(sim.last(), NO_ACTIONS);
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x02)));
    assert_eq!(sim.last(), ["Resolve(0)", "Resolve(2)"]);
    sim.run_until(ms(700));
    assert_eq!(
        sim.outcomes(),
        [Outcome::Confirmed, TIMEOUT, Outcome::Confirmed]
    );
    sim.check_all_tickets_resolved_once();
}

/// A sim in each phase outside `Up` that a command can meet, with a state that varies what `apply` reads.
fn outside_up() -> Vec<(&'static str, Sim)> {
    let mut idle = Sim::new();
    idle.state.noise_mode = NoiseMode::Adaptive;
    idle.state.model = Model::AirPodsPro3;

    let mut opening = Sim::answering(Answer::Hold);
    opening.feed(Input::Up(0));

    let mut handshake = Sim::new();
    handshake.feed(Input::Up(0));
    handshake.state.conversational_awareness = true;

    let mut backoff = Sim::answering(Answer::Refuse);
    backoff.feed(Input::Up(0));
    backoff.state.noise_mode = NoiseMode::Transparency;

    let mut grace = up_pro3(NoiseMode::Adaptive);
    grace.feed(Input::Down(0));
    assert!(matches!(grace.machine.phase(), Phase::Grace { .. }));

    vec![
        ("idle", idle),
        ("opening", opening),
        ("handshake", handshake),
        ("backoff", backoff),
        ("grace", grace),
    ]
}

#[test]
fn commands_outside_up_answer_as_before() {
    for command in EVERY_COMMAND {
        for (name, mut sim) in outside_up() {
            let phase = sim.machine.phase();
            let mut expected = sim.state.clone();
            let outcome = command::apply(&mut expected, &command);
            let tickets = sim.outcomes().len();
            sim.feed(Input::Command(command));
            assert_eq!(sim.last().len(), 1, "{name} {command:?}");
            assert_eq!(sim.outcomes()[tickets..], [outcome], "{name} {command:?}");
            assert_eq!(sim.state, expected, "{name} {command:?}");
            assert_eq!(sim.machine.phase(), phase, "{name} {command:?}");
            sim.check();
        }
    }
}

#[test]
fn commands_that_are_not_pod_settings_answer_in_up_as_without_a_link() {
    for command in EVERY_COMMAND {
        let setting = matches!(
            command,
            Command::SetNoise(_)
                | Command::SetConversationalAwareness(_)
                | Command::SetOneBudAnc(_)
                | Command::SetAdaptiveLevel(_)
        );
        if setting {
            continue;
        }
        let mut sim = up_pro3(NoiseMode::Anc);
        let mut expected = sim.state.clone();
        let outcome = command::apply(&mut expected, &command);
        sim.feed(Input::Command(command));
        assert_eq!(sim.last(), ["Resolve(0)"], "{command:?}");
        assert_eq!(sim.outcomes(), [outcome], "{command:?}");
        assert_eq!(sim.state, expected, "{command:?}");
        sim.check_all_tickets_resolved_once();
    }
}

#[test]
fn counters_rise_when_bytes_are_written() {
    let mut sim = up_pro3(NoiseMode::Anc);
    let before = sim.state.counters;
    for answered_at_once in [
        noise(NoiseMode::Off),
        noise(NoiseMode::Anc),
        Command::SetConversationalAwareness(false),
        Command::SetOneBudAnc(false),
        Command::SetAdaptiveLevel(30),
    ] {
        sim.feed(Input::Command(answered_at_once));
        assert_eq!(sim.last().len(), 1, "{answered_at_once:?}");
    }
    assert_eq!(sim.state.counters, before);

    for written in [
        noise(NoiseMode::Transparency),
        Command::SetConversationalAwareness(true),
        Command::SetOneBudAnc(true),
    ] {
        sim.feed(Input::Command(written));
    }
    sim.feed(Input::Packet(NoiseMode::Adaptive.command().encode()));
    sim.feed(Input::Command(Command::SetAdaptiveLevel(30)));
    let expected = Counters {
        noise_control_changes: 1,
        conversational_awareness_changes: 1,
        one_bud_anc_changes: 1,
        adaptive_level_changes: 1,
        ..before
    };
    assert_eq!(sim.state.counters, expected);
    sim.check();
}
