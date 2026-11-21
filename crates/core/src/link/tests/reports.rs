//! Packets to state: one test per row of the table, plus the bounds every packet keeps.

use pods_proto::battery::{Charge, Component};
use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::ear::{EarPlacement, EarReport};
use pods_proto::model::Model;
use pods_proto::noise::NoiseMode;
use pods_proto::packet::{Incoming, classify};

use super::sim::{Input, Sim, ms};
use crate::link::Phase;
use crate::link::ladder::Attempt;
use crate::state::State;

const NO_ACTIONS: [&str; 0] = [];
/// The longest random packet, one receive buffer.
const PACKET_MAX: usize = 4096;
/// Any fixed seed; the bytes only have to be arbitrary, not secret.
const RANDOM_SEED: u64 = 0x9e37_79b9_7f4a_7c15;

/// A control report from the pods with identifier `id` and first value byte `value`: `04 00 04 00 09 00 2e 1e 00 00 00` for the adaptive level 30.
pub(super) fn report(id: ControlId, value: u8) -> Vec<u8> {
    ControlCommand {
        id,
        value: [value, 0, 0, 0],
    }
    .encode()
}

/// The fields of `State` that differ between `before` and `after`, by name.
fn changed(before: &State, after: &State) -> Vec<&'static str> {
    let fields = [
        ("connected", before.connected != after.connected),
        ("device_name", before.device_name != after.device_name),
        ("model", before.model != after.model),
        ("model_number", before.model_number != after.model_number),
        ("noise_mode", before.noise_mode != after.noise_mode),
        (
            "adaptive_noise_level",
            before.adaptive_noise_level != after.adaptive_noise_level,
        ),
        (
            "conversational_awareness",
            before.conversational_awareness != after.conversational_awareness,
        ),
        (
            "one_bud_anc_mode",
            before.one_bud_anc_mode != after.one_bud_anc_mode,
        ),
        ("batteries", before.batteries != after.batteries),
        ("ear", before.ear != after.ear),
        ("primary", before.primary != after.primary),
        (
            "ear_detection_behavior",
            before.ear_detection_behavior != after.ear_detection_behavior,
        ),
        ("lid_state", before.lid_state != after.lid_state),
        ("counters", before.counters != after.counters),
    ];
    fields
        .into_iter()
        .filter(|(_, differs)| *differs)
        .map(|(name, _)| name)
        .collect()
}

/// The fields one report may change together; the link coming up is a change of its own.
const ONE_CHANGE: [&[&str]; 8] = [
    &["connected"],
    &["batteries", "primary"],
    &["ear"],
    &["noise_mode"],
    &["conversational_awareness"],
    &["one_bud_anc_mode"],
    &["adaptive_noise_level"],
    &["device_name", "model_number", "model"],
];

/// Arbitrary bytes from a fixed seed (xorshift64), so a failure replays.
fn random_bytes(len: usize) -> Vec<u8> {
    let mut seed = RANDOM_SEED;
    (0..len)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed.to_le_bytes()[0]
        })
        .collect()
}

#[test]
fn battery_ear_noise_and_metadata_reach_the_state() {
    let mut sim = Sim::new();
    sim.bring_up(0);

    sim.feed(Input::Packet(pods_samples::battery()));
    assert_eq!(sim.last(), NO_ACTIONS);
    let batteries = sim.state.batteries;
    assert_eq!(
        batteries.right,
        Some(Charge {
            level: 100,
            charging: false
        })
    );
    assert_eq!(
        batteries.left,
        Some(Charge {
            level: 99,
            charging: true
        })
    );
    assert_eq!(
        batteries.case,
        Some(Charge {
            level: 50,
            charging: false
        })
    );
    assert_eq!(batteries.headset, None);
    assert_eq!(sim.state.primary, Some(Component::Right));

    sim.feed(Input::Packet(pods_samples::ear()));
    assert_eq!(
        sim.state.ear,
        EarReport {
            primary: EarPlacement::InEar,
            secondary: EarPlacement::InEar
        }
    );

    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x03)));
    assert_eq!(sim.state.noise_mode, NoiseMode::Transparency);

    sim.feed(Input::Packet(pods_samples::control()));
    assert!(sim.state.conversational_awareness);
    sim.feed(Input::Packet(report(
        ControlId::CONVERSATIONAL_AWARENESS,
        0x02,
    )));
    assert!(!sim.state.conversational_awareness);

    sim.feed(Input::Packet(report(ControlId::ONE_BUD_ANC, 0x01)));
    assert!(sim.state.one_bud_anc_mode);
    sim.feed(Input::Packet(report(ControlId::ONE_BUD_ANC, 0x02)));
    assert!(!sim.state.one_bud_anc_mode);

    sim.feed(Input::Packet(report(ControlId::ADAPTIVE_NOISE_LEVEL, 30)));
    assert_eq!(sim.state.adaptive_noise_level, 30);

    sim.feed(Input::Packet(pods_samples::metadata()));
    assert_eq!(sim.state.device_name, "Test\u{2019}s AirPods Pro");
    assert_eq!(sim.state.model_number, "A3063");
    assert_eq!(sim.state.model, Model::AirPodsPro3);
    assert_eq!(sim.last(), NO_ACTIONS);
    sim.check();
}

#[test]
fn the_first_burst_fills_the_state() {
    let mut sim = Sim::new();
    sim.feed(Input::Up(0));
    // Anc first, so the burst's Off below is the burst's own report and not the default.
    sim.feed(Input::Packet(report(ControlId::NOISE_CONTROL, 0x02)));
    assert_eq!(sim.state.noise_mode, NoiseMode::Anc);
    for packet in pods_samples::first_burst() {
        sim.feed(Input::Packet(packet));
    }
    assert!(sim.state.connected);
    assert_eq!(sim.state.device_name, "Test\u{2019}s AirPods Pro");
    assert_eq!(sim.state.model_number, "A3063");
    assert_eq!(sim.state.model, Model::AirPodsPro3);
    let batteries = sim.state.batteries;
    assert!(batteries.left.is_some() && batteries.right.is_some() && batteries.case.is_some());
    assert!(sim.state.ear.any_in_ear());
    // The burst's setting 0x0D carries the value 01, which is Off.
    assert_eq!(sim.state.noise_mode, NoiseMode::Off);
    sim.check();
}

#[test]
fn reports_before_the_acknowledgement_are_applied_and_do_not_advance_the_handshake() {
    let mut sim = Sim::new();
    sim.feed(Input::Up(0));
    let waiting = Phase::AwaitHandshakeAck {
        attempt: Attempt::Held(1),
        until: ms(2000),
    };
    assert_eq!(sim.machine.phase(), waiting);

    sim.feed(Input::Packet(pods_samples::features_ack()));
    assert_eq!(sim.last(), NO_ACTIONS);
    assert_eq!(sim.machine.phase(), waiting);

    sim.feed(Input::Packet(pods_samples::battery()));
    assert_eq!(sim.last(), NO_ACTIONS);
    assert!(sim.state.batteries.left.is_some());
    assert_eq!(sim.machine.phase(), waiting);

    sim.feed(Input::Packet(pods_samples::ear()));
    assert!(sim.state.ear.any_in_ear());
    assert_eq!(sim.machine.phase(), waiting);
    assert!(!sim.state.connected);

    sim.feed(Input::Packet(pods_samples::handshake_ack()));
    assert_eq!(sim.last(), ["Send(features)"]);
    sim.feed(Input::Packet(pods_samples::metadata()));
    assert_eq!(sim.last(), NO_ACTIONS);
    assert_eq!(sim.state.model, Model::AirPodsPro3);
    assert!(matches!(
        sim.machine.phase(),
        Phase::AwaitFeaturesAck { .. }
    ));
    sim.check();
}

#[test]
fn one_packet_makes_at_most_one_change() {
    let mut packets = pods_samples::first_burst();
    packets.extend([
        pods_samples::control(),
        report(ControlId::ONE_BUD_ANC, 0x01),
        report(ControlId::ADAPTIVE_NOISE_LEVEL, 30),
        report(ControlId::NOISE_CONTROL, 0x04),
        pods_samples::battery(),
    ]);
    let mut sim = Sim::new();
    sim.feed(Input::Up(0));
    let mut changes = 0;
    for packet in packets {
        let before = sim.state.clone();
        sim.feed(Input::Packet(packet.clone()));
        let fields = changed(&before, &sim.state);
        assert_eq!(fields.is_empty(), before == sim.state, "{packet:02x?}");
        assert!(
            fields.is_empty()
                || ONE_CHANGE
                    .iter()
                    .any(|group| fields.iter().all(|field| group.contains(field))),
            "{packet:02x?} changed {fields:?}"
        );
        if !fields.is_empty() {
            changes += 1;
        }
    }
    // Up, battery, ear, metadata, then the four added reports; the second battery repeats the first.
    assert_eq!(changes, 8);
    sim.check();
}

#[test]
fn unknown_opcodes_and_bad_packets_change_nothing() {
    let mut packets: Vec<Vec<u8>> = pods_samples::first_burst()
        .into_iter()
        .filter(|packet| matches!(classify(packet), Ok(Incoming::Other { .. })))
        .collect();
    assert_eq!(packets.len(), 12);
    let mut short_battery = pods_samples::battery();
    short_battery.truncate(10);
    packets.extend([
        short_battery,
        random_bytes(PACKET_MAX),
        pods_samples::handshake_ack(),
        // An uplink packet, left for the audio path.
        vec![0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01, 0x02],
        // A setting no row reads, a noise value outside the modes, a switch that is neither on nor off.
        report(ControlId::MIC_SIDE, 0x01),
        report(ControlId::NOISE_CONTROL, 0x09),
        report(ControlId::CONVERSATIONAL_AWARENESS, 0x03),
        report(ControlId::ONE_BUD_ANC, 0x00),
        Vec::new(),
    ]);
    // Switches on then off (01, 02) and the mode set both ways, so a bad value read as either shows as a change.
    for (switch, on, mode) in [
        (0x01, true, NoiseMode::Transparency),
        (0x02, false, NoiseMode::Off),
    ] {
        let mut sim = Sim::new();
        sim.bring_up(0);
        for packet in [
            pods_samples::battery(),
            pods_samples::ear(),
            pods_samples::metadata(),
            report(ControlId::CONVERSATIONAL_AWARENESS, switch),
            report(ControlId::ONE_BUD_ANC, switch),
            mode.command().encode(),
        ] {
            sim.feed(Input::Packet(packet));
        }
        assert_eq!(sim.state.conversational_awareness, on);
        assert_eq!(sim.state.one_bud_anc_mode, on);
        assert_eq!(sim.state.noise_mode, mode);
        for packet in &packets {
            let before = sim.state.clone();
            let phase = sim.machine.phase();
            sim.feed(Input::Packet(packet.clone()));
            assert_eq!(sim.state, before, "{packet:02x?}");
            assert_eq!(sim.last(), NO_ACTIONS, "{packet:02x?}");
            assert_eq!(sim.machine.phase(), phase, "{packet:02x?}");
        }
        sim.check();
    }
}

#[test]
fn notifications_are_asked_again_once_after_2_s_without_battery() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.run_until(ms(1999));
    sim.feed(Input::Packet(pods_samples::battery()));
    assert_eq!(sim.machine.deadline(), None);
    sim.run_until(ms(4000));
    assert_eq!(sim.times_of("Send(notifications)"), [0]);
    sim.check();

    // A battery report during the handshake also counts, until the next channel opens.
    let mut sim = Sim::new();
    sim.feed(Input::Up(0));
    sim.feed(Input::Packet(pods_samples::battery()));
    sim.feed(Input::Packet(pods_samples::handshake_ack()));
    sim.feed(Input::Packet(pods_samples::features_ack()));
    assert_eq!(sim.machine.phase(), Phase::Up { rerequest: None });
    sim.run_until(ms(4000));
    assert_eq!(sim.times_of("Send(notifications)"), [0]);
    sim.feed(Input::PeerCloses);
    sim.run_until(ms(4750));
    sim.feed(Input::Packet(pods_samples::handshake_ack()));
    sim.feed(Input::Packet(pods_samples::features_ack()));
    sim.run_until(ms(10_000));
    assert_eq!(sim.times_of("Send(notifications)"), [0, 4750, 6750]);
    sim.check();
}

#[test]
fn an_adaptive_level_over_100_from_the_pods_is_ignored() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    let level = sim.state.adaptive_noise_level;
    for ignored in [101, 0x7f, u8::MAX] {
        sim.feed(Input::Packet(report(
            ControlId::ADAPTIVE_NOISE_LEVEL,
            ignored,
        )));
        assert_eq!(sim.state.adaptive_noise_level, level, "{ignored}");
    }
    sim.feed(Input::Packet(report(ControlId::ADAPTIVE_NOISE_LEVEL, 100)));
    assert_eq!(sim.state.adaptive_noise_level, 100);
    sim.feed(Input::Packet(report(ControlId::ADAPTIVE_NOISE_LEVEL, 0)));
    assert_eq!(sim.state.adaptive_noise_level, 0);
    sim.check();
}
