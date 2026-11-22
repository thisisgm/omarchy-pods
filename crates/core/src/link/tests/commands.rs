//! Commands to bytes: one test per row and column of the table, the answer at the write, and the hold against late reports.

use pods_proto::control::ControlId;
use pods_proto::model::Model;
use pods_proto::noise::NoiseMode;

use super::reports::report;
use super::sim::{Answer, Input, Sim, ms};
use crate::command::tests::level;
use crate::command::{self, Command, CommandError, NoiseTarget, Outcome};
use crate::link::{Phase, REPORT_HOLD_MS};
use crate::state::{Counters, EarDetectionBehavior, State};

const OFF_FRAME: &str = "Send([04, 00, 04, 00, 09, 00, 0d, 01, 00, 00, 00])";
const ANC_FRAME: &str = "Send([04, 00, 04, 00, 09, 00, 0d, 02, 00, 00, 00])";
const TRANSPARENCY_FRAME: &str = "Send([04, 00, 04, 00, 09, 00, 0d, 03, 00, 00, 00])";
const ADAPTIVE_FRAME: &str = "Send([04, 00, 04, 00, 09, 00, 0d, 04, 00, 00, 00])";
const NOT_SUPPORTED: Outcome = Outcome::Refused(CommandError::NotSupported);
const NOT_CONNECTED: Outcome = Outcome::Refused(CommandError::NotConnected);

/// Both buds out of the ear: header 04 00 04 00 06 00, then primary 01 and secondary 01.
const OUT_OF_EAR: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x01, 0x01];
/// Both buds in the case.
const IN_CASE: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x02, 0x02];
/// The primary bud in an ear, the secondary out.
const PRIMARY_IN_EAR: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x01];
/// The secondary bud in an ear, the primary out.
const SECONDARY_IN_EAR: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x01, 0x00];

/// Every command a client can send but connect, disconnect and forget, which `devices.rs` covers.
fn every_command() -> [Command; 14] {
    [
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
        Command::SetAdaptiveLevel(level(30)),
        Command::Reopen,
    ]
}

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

/// Fails unless the last two steps wrote `frame` and answered ticket `ticket` as sent.
fn written_and_sent(sim: &Sim, frame: &str, ticket: u64) {
    assert_eq!(
        sim.since(2),
        [frame.to_owned(), format!("Resolve({ticket})")]
    );
    assert_eq!(sim.outcome(ticket), [Outcome::Sent]);
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
fn noise_anc_is_written_then_sent_and_shown() {
    let mut sim = up_unknown();
    sim.feed(Input::Command(noise(NoiseMode::Anc)));
    written_and_sent(&sim, ANC_FRAME, 0);
    assert_eq!(sim.steps.last().unwrap().event, "Tick");
    assert_eq!(sim.state.noise_mode, NoiseMode::Anc);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_mode_already_shown_is_written_again() {
    let mut sim = up_unknown();
    sim.feed(Input::Packet(NoiseMode::Off.command().encode()));
    sim.feed(Input::Command(noise(NoiseMode::Off)));
    written_and_sent(&sim, OFF_FRAME, 0);
    let mut sim3 = up_pro3(NoiseMode::Adaptive);
    sim3.feed(Input::Command(noise(NoiseMode::Adaptive)));
    written_and_sent(&sim3, ADAPTIVE_FRAME, 0);
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
fn a_late_report_of_another_value_is_dropped_until_the_written_one() {
    let mut sim = up_pro3(NoiseMode::Transparency);
    sim.feed(Input::Command(noise(NoiseMode::Anc)));
    sim.feed(Input::Command(noise(NoiseMode::Adaptive)));
    // The written value under another identifier does not end the hold.
    sim.feed(Input::Packet(report(ControlId::MIC_SIDE, 0x04)));
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x02)));
    assert_eq!(sim.state.noise_mode, NoiseMode::Adaptive);
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x04)));
    // The written value ended the hold, so the pods' own change is taken at once.
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x03)));
    assert_eq!(sim.state.noise_mode, NoiseMode::Transparency);
    sim.run_until(ms(REPORT_HOLD_MS + 1));
    assert_eq!(sim.state.noise_mode, NoiseMode::Transparency);
    assert_eq!(sim.outcomes(), [Outcome::Sent; 2]);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_report_of_another_value_held_back_is_taken_when_the_hold_passes() {
    let mut sim = up_pro3(NoiseMode::Transparency);
    sim.run_until(ms(100));
    sim.feed(Input::Command(Command::SetConversationalAwareness(true)));
    sim.run_until(ms(100 + REPORT_HOLD_MS - 2));
    let off = report(ControlId::CONVERSATIONAL_AWARENESS, 0x02);
    sim.feed(Input::Packet(off));
    sim.run_until(ms(100 + REPORT_HOLD_MS - 1));
    assert!(sim.state.conversational_awareness);
    sim.run_until(ms(100 + REPORT_HOLD_MS));
    assert!(!sim.state.conversational_awareness);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn the_last_of_several_reports_held_back_is_taken_when_the_hold_passes() {
    let mut sim = up_pro3(NoiseMode::Transparency);
    sim.feed(Input::Command(noise(NoiseMode::Anc)));
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x03)));
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x04)));
    assert_eq!(sim.state.noise_mode, NoiseMode::Anc);
    sim.run_until(ms(REPORT_HOLD_MS));
    assert_eq!(sim.state.noise_mode, NoiseMode::Adaptive);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_report_held_back_survives_a_later_write_and_is_taken_when_the_last_hold_passes() {
    let mut sim = up_pro3(NoiseMode::Transparency);
    sim.feed(Input::Command(noise(NoiseMode::Anc)));
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x03)));
    let second_write_at = REPORT_HOLD_MS / 2;
    sim.run_until(ms(second_write_at));
    sim.feed(Input::Command(noise(NoiseMode::Adaptive)));
    sim.run_until(ms(second_write_at + REPORT_HOLD_MS - 1));
    assert_eq!(sim.state.noise_mode, NoiseMode::Adaptive);
    sim.run_until(ms(second_write_at + REPORT_HOLD_MS));
    assert_eq!(sim.state.noise_mode, NoiseMode::Transparency);
    sim.check_all_tickets_resolved_once();
}

/// One held setting: the command that writes it, its identifier, the written, a valid other and an invalid wire value, and the value shown.
struct HeldRow {
    write: Command,
    id: ControlId,
    written: u8,
    valid: u8,
    invalid: u8,
    shown: fn(&State) -> u8,
}

/// The wire value of a switch setting: 01 enabled, 02 disabled.
fn switch_value(enabled: bool) -> u8 {
    if enabled { 0x01 } else { 0x02 }
}

/// Each of the four held settings, with an invalid value past its valid range.
fn held_rows() -> [HeldRow; 4] {
    [
        HeldRow {
            write: noise(NoiseMode::Anc),
            id: ControlId::NOISE_CONTROL,
            written: 0x02,
            valid: 0x03,
            invalid: 0xff,
            shown: |state| state.noise_mode.command().value[0],
        },
        HeldRow {
            write: Command::SetConversationalAwareness(true),
            id: ControlId::CONVERSATIONAL_AWARENESS,
            written: 0x01,
            valid: 0x02,
            invalid: 0x03,
            shown: |state| switch_value(state.conversational_awareness),
        },
        HeldRow {
            write: Command::SetOneBudAnc(true),
            id: ControlId::ONE_BUD_ANC,
            written: 0x01,
            valid: 0x02,
            invalid: 0x03,
            shown: |state| switch_value(state.one_bud_anc_mode),
        },
        HeldRow {
            write: Command::SetAdaptiveLevel(level(30)),
            id: ControlId::ADAPTIVE_NOISE_LEVEL,
            written: 30,
            valid: 70,
            invalid: 0xff,
            shown: |state| state.adaptive_noise_level.percent(),
        },
    ]
}

#[test]
fn an_invalid_report_held_back_never_replaces_the_valid_one_before_it() {
    for row in held_rows() {
        let mut sim = up_pro3(NoiseMode::Adaptive);
        sim.feed(Input::Command(row.write));
        sim.feed(Input::Packet(report(row.id, row.valid)));
        sim.feed(Input::Packet(report(row.id, row.invalid)));
        sim.run_until(ms(REPORT_HOLD_MS - 1));
        assert_eq!((row.shown)(&sim.state), row.written, "{:?}", row.id);
        sim.run_until(ms(REPORT_HOLD_MS));
        assert_eq!((row.shown)(&sim.state), row.valid, "{:?}", row.id);
        sim.check_all_tickets_resolved_once();
    }
}

#[test]
fn an_invalid_report_inside_a_hold_leaves_the_written_value() {
    for row in held_rows() {
        let mut sim = up_pro3(NoiseMode::Adaptive);
        sim.feed(Input::Command(row.write));
        sim.feed(Input::Packet(report(row.id, row.invalid)));
        sim.run_until(ms(REPORT_HOLD_MS + 1));
        assert_eq!((row.shown)(&sim.state), row.written, "{:?}", row.id);
        sim.check_all_tickets_resolved_once();
    }
}

#[test]
fn an_invalid_report_with_no_hold_changes_nothing() {
    for row in held_rows() {
        let mut sim = up_pro3(NoiseMode::Adaptive);
        let before = sim.state.clone();
        sim.feed(Input::Packet(report(row.id, row.invalid)));
        assert_eq!(sim.state, before, "{:?}", row.id);
    }
}

#[test]
fn a_report_with_no_write_of_its_setting_is_taken_at_once() {
    let mut sim = up_pro3(NoiseMode::Transparency);
    sim.feed(Input::Command(noise(NoiseMode::Anc)));
    sim.feed(Input::Packet(report(ControlId::ONE_BUD_ANC, 0x01)));
    assert!(sim.state.one_bud_anc_mode);
    sim.feed(Input::Packet(report(
        ControlId::CONVERSATIONAL_AWARENESS,
        0x01,
    )));
    assert!(sim.state.conversational_awareness);
    assert_eq!(sim.state.noise_mode, NoiseMode::Anc);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn noise_with_no_bud_in_an_ear_is_not_worn_and_never_written() {
    let not_worn = Outcome::Refused(CommandError::NotWorn);
    for placement in [OUT_OF_EAR, IN_CASE] {
        let mut sim = up_unknown();
        sim.feed(Input::Packet(placement.to_vec()));
        sim.feed(Input::Command(noise(NoiseMode::Anc)));
        assert_eq!(sim.last(), ["Resolve(0)"]);
        sim.feed(Input::Command(Command::SetNoise(NoiseTarget::Cycle)));
        assert_eq!(sim.last(), ["Resolve(1)"]);
        let no_step: [u64; 0] = [];
        assert_eq!(sim.times_of(ANC_FRAME), no_step);
        assert_eq!(sim.times_of(TRANSPARENCY_FRAME), no_step);
        assert_eq!(sim.outcomes(), [not_worn, not_worn]);
        assert_eq!(sim.state.noise_mode, NoiseMode::Off);
        sim.check_all_tickets_resolved_once();
    }
    // One bud in an ear is worn, and a headset has no ear placement to ask about.
    let mut worn = Vec::new();
    for placement in [PRIMARY_IN_EAR, SECONDARY_IN_EAR] {
        let mut sim = up_unknown();
        sim.feed(Input::Packet(placement.to_vec()));
        worn.push(sim);
    }
    let mut headset = up_unknown();
    headset.feed(Input::Packet(OUT_OF_EAR.to_vec()));
    headset.state.model = Model::AirPodsMaxUSBC;
    worn.push(headset);
    for mut sim in worn {
        sim.feed(Input::Command(noise(NoiseMode::Anc)));
        written_and_sent(&sim, ANC_FRAME, 0);
        sim.check_all_tickets_resolved_once();
    }
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
        assert_eq!(sim.since(2)[0], frame, "Pro 3 from {mode:?}");
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
        assert_eq!(sim.since(2)[0], frame, "unknown model from {mode:?}");
        sim.check();
    }
    // A model with both Off and Adaptive goes from Adaptive to Off.
    let mut sim = up_pro3(NoiseMode::Adaptive);
    sim.state.model = Model::AirPodsPro2USBC;
    sim.feed(Input::Command(cycle));
    assert_eq!(sim.since(2)[0], OFF_FRAME);
    sim.check();

    // A second cycle steps from the mode the first one wrote.
    let mut sim = up_pro3(NoiseMode::Anc);
    for frame in [TRANSPARENCY_FRAME, ADAPTIVE_FRAME, ANC_FRAME] {
        sim.feed(Input::Command(cycle));
        assert_eq!(sim.since(2)[0], frame);
    }
    assert_eq!(sim.outcomes(), [Outcome::Sent; 3]);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn conversational_awareness_and_one_bud_are_written_on_every_ask() {
    let mut sim = up_pro3(NoiseMode::Anc);
    let ca_on = "Send([04, 00, 04, 00, 09, 00, 28, 01, 00, 00, 00])";
    let ca_off = "Send([04, 00, 04, 00, 09, 00, 28, 02, 00, 00, 00])";
    let one_bud_on = "Send([04, 00, 04, 00, 09, 00, 1b, 01, 00, 00, 00])";
    let one_bud_off = "Send([04, 00, 04, 00, 09, 00, 1b, 02, 00, 00, 00])";
    for (ticket, command, frame, shown) in [
        (
            0,
            Command::SetConversationalAwareness(true),
            ca_on,
            (true, false),
        ),
        (
            1,
            Command::SetConversationalAwareness(true),
            ca_on,
            (true, false),
        ),
        (2, Command::SetOneBudAnc(true), one_bud_on, (true, true)),
        (3, Command::SetOneBudAnc(false), one_bud_off, (true, false)),
        (
            4,
            Command::SetConversationalAwareness(false),
            ca_off,
            (false, false),
        ),
    ] {
        sim.feed(Input::Command(command));
        written_and_sent(&sim, frame, ticket);
        let state = (
            sim.state.conversational_awareness,
            sim.state.one_bud_anc_mode,
        );
        assert_eq!(state, shown, "{command:?}");
    }
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
fn adaptive_level_is_stored_and_sent_in_the_step_after_its_write() {
    let mut sim = up_pro3(NoiseMode::Adaptive);
    sim.feed(Input::Command(Command::SetAdaptiveLevel(level(30))));
    assert_eq!(
        sim.since(2),
        [
            "Send([04, 00, 04, 00, 09, 00, 2e, 1e, 00, 00, 00])",
            "Resolve(0)"
        ]
    );
    assert_eq!(sim.steps.last().unwrap().event, "Tick");
    assert_eq!(sim.outcomes(), [Outcome::Sent]);
    assert_eq!(sim.state.adaptive_noise_level, level(30));
    assert_eq!(sim.state.counters.adaptive_level_changes, 1);

    sim.feed(Input::Command(Command::SetAdaptiveLevel(level(30))));
    written_and_sent(
        &sim,
        "Send([04, 00, 04, 00, 09, 00, 2e, 1e, 00, 00, 00])",
        1,
    );
    assert_eq!(sim.state.counters.adaptive_level_changes, 2);

    sim.check_all_tickets_resolved_once();
}

#[test]
fn a_setting_whose_write_fails_is_refused_and_never_stored() {
    for command in [
        noise(NoiseMode::Transparency),
        Command::SetAdaptiveLevel(level(30)),
    ] {
        let mut sim = up_pro3(NoiseMode::Adaptive);
        let before = sim.state.clone();
        sim.feed(Input::NextWriteFails);
        sim.feed(Input::Command(command));
        assert_eq!(sim.step_of("SendFailed").actions, ["Resolve(0)", "Close"]);
        assert_eq!(sim.outcome(0), [NOT_CONNECTED], "{command:?}");
        assert_eq!(sim.state.noise_mode, before.noise_mode, "{command:?}");
        assert_eq!(sim.state.adaptive_noise_level, before.adaptive_noise_level);
        assert_eq!(sim.state.counters, before.counters, "{command:?}");
        sim.feed(Input::Command(command));
        assert_eq!(sim.outcome(1), [NOT_CONNECTED], "{command:?}");
        sim.check_all_tickets_resolved_once();
    }
}

#[test]
fn a_handshake_write_that_fails_closes_and_climbs_the_ladder() {
    let mut sim = Sim::new();
    sim.feed(Input::NextWriteFails);
    sim.feed(Input::Up(0));
    assert_eq!(sim.step_of("SendFailed").actions, ["Close"]);
    assert!(matches!(sim.machine.phase(), Phase::Backoff { .. }));
    sim.check();
}

#[test]
fn adaptive_level_outside_adaptive_mode_is_not_supported() {
    let mut sim = up_pro3(NoiseMode::Anc);
    let before = sim.state.adaptive_noise_level;
    sim.feed(Input::Command(Command::SetAdaptiveLevel(level(30))));
    assert_eq!(sim.last(), ["Resolve(0)"]);

    let mut unknown = up_unknown();
    unknown.feed(Input::Packet(NoiseMode::Adaptive.command().encode()));
    assert_eq!(unknown.state.noise_mode, NoiseMode::Adaptive);
    let unknown_level = unknown.state.adaptive_noise_level;
    unknown.feed(Input::Command(Command::SetAdaptiveLevel(level(30))));
    assert_eq!(unknown.last(), ["Resolve(0)"]);

    assert_eq!(sim.outcomes(), [NOT_SUPPORTED]);
    assert_eq!(unknown.outcomes(), [NOT_SUPPORTED]);
    assert_eq!(sim.state.adaptive_noise_level, before);
    assert_eq!(unknown.state.adaptive_noise_level, unknown_level);
    assert_eq!(sim.state.counters.adaptive_level_changes, 0);
    sim.check_all_tickets_resolved_once();
    unknown.check_all_tickets_resolved_once();
}

/// A sim in each phase outside `Up` that a command can meet, with a state that varies what `apply` reads.
fn outside_up() -> Vec<(&'static str, Sim)> {
    let mut idle = Sim::new();
    idle.state.noise_mode = NoiseMode::Adaptive;
    idle.state.model = Model::AirPodsPro3;
    assert!(matches!(idle.machine.phase(), Phase::Idle));

    let mut opening = Sim::answering(Answer::Hold);
    opening.feed(Input::Up(0));
    assert!(matches!(opening.machine.phase(), Phase::Opening { .. }));

    let mut handshake = Sim::new();
    handshake.feed(Input::Up(0));
    handshake.state.conversational_awareness = true;
    assert!(matches!(
        handshake.machine.phase(),
        Phase::AwaitHandshakeAck { .. }
    ));

    let mut backoff = Sim::answering(Answer::Refuse);
    backoff.feed(Input::Up(0));
    backoff.state.noise_mode = NoiseMode::Transparency;
    assert!(matches!(backoff.machine.phase(), Phase::Backoff { .. }));

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
    for command in every_command() {
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
    for command in every_command() {
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
fn counters_rise_with_each_write_and_never_with_a_refusal() {
    let mut sim = up_pro3(NoiseMode::Anc);
    let before = sim.state.counters;
    for refused in [noise(NoiseMode::Off), Command::SetAdaptiveLevel(level(30))] {
        sim.feed(Input::Command(refused));
        assert_eq!(sim.last().len(), 1, "{refused:?}");
    }
    assert_eq!(sim.state.counters, before);
    for written in [
        noise(NoiseMode::Anc),
        noise(NoiseMode::Adaptive),
        Command::SetConversationalAwareness(false),
        Command::SetOneBudAnc(false),
        Command::SetAdaptiveLevel(level(30)),
    ] {
        sim.feed(Input::Command(written));
    }
    let expected = Counters {
        noise_control_changes: 2,
        conversational_awareness_changes: 1,
        one_bud_anc_changes: 1,
        adaptive_level_changes: 1,
        ..before
    };
    assert_eq!(sim.state.counters, expected);
    sim.check();
}
