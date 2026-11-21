//! The session lifecycle, one test per transition or pair of transitions.

use cpp_source::DEVICE_INFO;
use pods_proto::addr::BdAddr;
use pods_proto::battery::{Batteries, Charge, Component};
use pods_proto::ear::{EarPlacement, EarReport};
use pods_proto::model::Model;
use pods_proto::noise::NoiseMode;

use super::sim::{Answer, Input, Sim, addr, ms};
use crate::command::{Command, CommandError, NoiseTarget, Outcome};
use crate::link::ladder::KNOWN_MAX;
use crate::link::{Event, Machine, Phase};
use crate::state::{EarDetectionBehavior, LidState, State};

const NO_ACTIONS: [&str; 0] = [];

#[test]
fn handshake_sends_the_three_requests_in_order_and_goes_up() {
    let mut sim = Sim::new();
    sim.feed(Input::Up(0));
    assert_eq!(sim.step_of("Up(0)").actions, ["Open(0)"]);
    assert_eq!(sim.step_of("Opened").actions, ["Send(handshake)"]);
    assert_eq!(
        sim.machine.phase(),
        Phase::AwaitHandshakeAck {
            attempt: 1,
            until: ms(2000)
        }
    );
    sim.feed(Input::Packet(pods_samples::ear()));
    assert_eq!(sim.last(), NO_ACTIONS);
    sim.run_until(ms(300));
    sim.feed(Input::Packet(pods_samples::handshake_ack()));
    assert_eq!(sim.last(), ["Send(features)"]);
    assert_eq!(
        sim.machine.phase(),
        Phase::AwaitFeaturesAck {
            attempt: 1,
            until: ms(2300)
        }
    );
    assert!(!sim.state.connected);
    sim.feed(Input::Packet(pods_samples::features_ack()));
    assert_eq!(sim.last(), ["Send(notifications)"]);
    assert_eq!(
        sim.machine.phase(),
        Phase::Up {
            rerequest: Some(ms(2300))
        }
    );
    assert!(sim.state.connected);
    assert!(sim.machine.populated);
    assert_eq!(
        sim.machine.last_linked.map(BdAddr::octets),
        Some(addr(0).octets())
    );
    sim.check();
}

#[test]
fn the_first_burst_brings_the_link_up_once() {
    let mut sim = Sim::new();
    sim.feed(Input::Up(0));
    for packet in pods_samples::first_burst() {
        sim.feed(Input::Packet(packet));
    }
    let sends: Vec<String> = sim
        .actions()
        .into_iter()
        .map(|(_, action)| action)
        .filter(|action| action.starts_with("Send"))
        .collect();
    assert_eq!(
        sends,
        ["Send(handshake)", "Send(features)", "Send(notifications)"]
    );
    assert!(sim.state.connected);
    sim.check();
}

#[test]
fn up_repeats_the_notification_request_once() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.run_until(ms(1999));
    assert_eq!(sim.times_of("Send(notifications)"), [0]);
    sim.run_until(ms(2000));
    assert_eq!(sim.times_of("Send(notifications)"), [0, 2000]);
    assert_eq!(sim.machine.deadline(), None);
    sim.run_until(ms(600_000));
    assert_eq!(sim.times_of("Send(notifications)"), [0, 2000]);
    assert_eq!(sim.machine.phase(), Phase::Up { rerequest: None });
    sim.check();
}

#[test]
fn a_features_ack_that_never_comes_closes_the_attempt() {
    let mut sim = Sim::new();
    sim.feed(Input::NextOpen(Answer::Open));
    sim.feed(Input::NextOpen(Answer::Hold));
    sim.feed(Input::Up(0));
    sim.run_until(ms(500));
    sim.feed(Input::Packet(pods_samples::handshake_ack()));
    sim.run_until(ms(2499));
    assert_eq!(sim.times_of("Close"), Vec::<u64>::new());
    sim.run_until(ms(2500));
    assert_eq!(sim.times_of("Close"), [2500]);
    assert_eq!(
        sim.machine.phase(),
        Phase::Backoff {
            attempt: 2,
            until: ms(3500)
        }
    );
    sim.check();
}

#[test]
fn a_duplicate_device_up_does_not_reset_the_ladder() {
    let mut sim = Sim::answering(Answer::Refuse);
    sim.feed(Input::Up(0));
    sim.run_until(ms(3500));
    let backoff = Phase::Backoff {
        attempt: 4,
        until: ms(7000),
    };
    assert_eq!(sim.machine.phase(), backoff);
    sim.feed(Input::Up(0));
    assert_eq!(sim.last(), NO_ACTIONS);
    assert_eq!(sim.machine.phase(), backoff);
    sim.run_until(ms(7000));
    assert_eq!(sim.times_of("Open(0)"), [0, 1000, 3000, 7000]);

    let mut sim = Sim::new();
    sim.bring_up(0);
    let up = sim.machine.phase();
    sim.feed(Input::Up(0));
    assert_eq!(sim.last(), NO_ACTIONS);
    assert_eq!(sim.machine.phase(), up);
    sim.check();
}

#[test]
fn a_second_device_waits_for_the_first() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.state.device_name = "Test AirPods".into();
    let knows_the_second = |sim: &Sim| {
        sim.machine
            .known
            .iter()
            .any(|known| known.octets() == addr(1).octets())
    };
    sim.feed(Input::Up(1));
    assert_eq!(sim.last(), NO_ACTIONS);
    assert!(knows_the_second(&sim));
    sim.feed(Input::Down(1));
    sim.feed(Input::Up(1));
    assert_eq!(sim.last(), NO_ACTIONS);
    assert!(matches!(sim.machine.phase(), Phase::Up { .. }));

    sim.feed(Input::Gone(0));
    assert_eq!(sim.step_of("Gone(0)").actions, ["Close", "Sweep"]);
    assert_eq!(sim.step_of("Ended(Closed)").actions, ["Sweep"]);
    let actions = sim.actions();
    let ended = actions
        .iter()
        .rposition(|(_, action)| action == "Sweep")
        .unwrap();
    let opened = actions
        .iter()
        .position(|(_, action)| action == "Open(1)")
        .unwrap();
    assert!(opened > ended);
    assert_eq!(sim.state.device_name, "");
    assert!(
        !sim.machine
            .known
            .iter()
            .any(|known| known.octets() == addr(0).octets())
    );
    assert!(matches!(
        sim.machine.phase(),
        Phase::AwaitHandshakeAck { attempt: 1, .. }
    ));
    sim.check();
}

#[test]
fn known_keeps_the_newest_devices_up_to_its_bound() {
    let mut machine = Machine::new();
    let mut state = State::default();
    let _actions = machine.step(&mut state, Event::DeviceUp(addr(0)), ms(0));
    let others: Vec<BdAddr> = (0..20)
        .map(|n| format!("02:00:00:00:01:{n:02x}").parse().unwrap())
        .collect();
    for other in &others {
        let _actions = machine.step(&mut state, Event::DeviceUp(*other), ms(0));
        let _actions = machine.step(&mut state, Event::DeviceDown(*other), ms(0));
    }
    assert_eq!(machine.known.len(), 16);
    assert_eq!(KNOWN_MAX, 16);
    let newest: Vec<_> = others[others.len() - KNOWN_MAX..]
        .iter()
        .map(|a| a.octets())
        .collect();
    let known: Vec<_> = machine.known.iter().map(|a| a.octets()).collect();
    assert_eq!(known, newest);
}

#[test]
fn device_down_while_opening_waits_for_the_end() {
    let mut sim = Sim::answering(Answer::Hold);
    sim.feed(Input::Up(0));
    sim.feed(Input::Down(0));
    assert_eq!(sim.last(), NO_ACTIONS);
    sim.feed(Input::Up(0));
    assert_eq!(sim.last(), NO_ACTIONS);
    assert_eq!(sim.machine.phase(), Phase::Opening { attempt: 1 });
    sim.feed(Input::Release(false));
    assert_eq!(sim.times_of("Open(0)"), [0]);
    assert_eq!(
        sim.machine.phase(),
        Phase::Backoff {
            attempt: 2,
            until: ms(1000)
        }
    );

    let mut sim = Sim::answering(Answer::Hold);
    sim.feed(Input::Up(0));
    sim.feed(Input::Down(0));
    sim.feed(Input::Release(false));
    assert_eq!(sim.last(), ["Sweep"]);
    assert_eq!(sim.machine.phase(), Phase::Idle);
    sim.check();
}

#[test]
fn device_gone_resets_at_once() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.state.device_name = "Test AirPods".into();
    sim.feed(Input::Gone(0));
    assert_eq!(sim.step_of("Gone(0)").actions, ["Close", "Sweep"]);
    assert_eq!(sim.machine.phase(), Phase::Idle);
    assert_eq!(sim.state.device_name, "");
    assert!(!sim.state.connected);
    assert!(sim.machine.last_linked.is_none());
    assert!(sim.machine.known.is_empty());

    let mut sim = Sim::answering(Answer::Refuse);
    sim.feed(Input::Up(0));
    sim.feed(Input::Gone(0));
    assert_eq!(sim.last(), ["Sweep"]);
    assert_eq!(sim.machine.phase(), Phase::Idle);

    let mut sim = Sim::answering(Answer::Hold);
    sim.feed(Input::Up(0));
    sim.state.device_name = "Test AirPods".into();
    sim.feed(Input::Gone(0));
    assert_eq!(sim.last(), ["Sweep"]);
    assert_eq!(sim.state.device_name, "");
    assert_eq!(sim.machine.phase(), Phase::Closing { attempt: None });
    sim.feed(Input::Release(true));
    assert_eq!(sim.step_of("Opened").actions, ["Close"]);
    assert_eq!(sim.step_of("Ended(Closed)").actions, ["Sweep"]);
    assert_eq!(sim.machine.phase(), Phase::Idle);
    sim.check();
}

#[test]
fn bluez_lost_while_up_keeps_the_channel() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.feed(Input::BluezLost);
    assert_eq!(sim.last(), NO_ACTIONS);
    assert!(matches!(sim.machine.phase(), Phase::Up { .. }));
    assert!(sim.state.connected);
    sim.feed(Input::PeerCloses);
    assert_eq!(sim.machine.phase(), Phase::Grace { until: ms(7750) });

    let mut sim = Sim::answering(Answer::Refuse);
    sim.feed(Input::Up(0));
    sim.feed(Input::BluezLost);
    assert_eq!(sim.last(), ["Sweep"]);
    assert_eq!(sim.machine.phase(), Phase::Idle);
    sim.check();
}

#[test]
fn a_channel_that_opens_after_the_device_left_is_closed() {
    let mut sim = Sim::answering(Answer::Hold);
    sim.feed(Input::Up(0));
    sim.feed(Input::Down(0));
    sim.feed(Input::Release(true));
    assert_eq!(sim.step_of("Opened").actions, ["Close"]);
    assert_eq!(sim.step_of("Ended(Closed)").actions, ["Sweep"]);
    assert_eq!(sim.machine.phase(), Phase::Idle);
    sim.check();
}

#[test]
fn commands_are_answered_at_once_in_every_phase() {
    let mut sim = Sim::answering(Answer::Hold);
    let ear = Command::SetEarDetection(EarDetectionBehavior::Disabled);
    let noise = Command::SetNoise(NoiseTarget::Cycle);
    sim.feed(Input::Command(ear));
    sim.feed(Input::Up(0));
    sim.feed(Input::Command(noise));
    sim.feed(Input::Release(true));
    sim.feed(Input::Packet(pods_samples::handshake_ack()));
    sim.feed(Input::Packet(pods_samples::features_ack()));
    sim.feed(Input::Command(noise));
    assert_eq!(sim.last(), ["Resolve(2)"]);
    let refused = Outcome::Refused(CommandError::NotConnected);
    assert_eq!(sim.outcomes(), [Outcome::Confirmed, refused, refused]);
    assert_eq!(
        sim.state.ear_detection_behavior,
        EarDetectionBehavior::Disabled
    );
    sim.feed(Input::CallFailed);
    assert_eq!(sim.last(), NO_ACTIONS);
    sim.check_all_tickets_resolved_once();
}

#[test]
fn shutdown_closes_and_ignores_everything_after() {
    let mut sim = Sim::new();
    sim.bring_up(0);
    sim.feed(Input::Shutdown);
    assert_eq!(sim.step_of("Shutdown").actions, ["Close"]);
    assert_eq!(sim.step_of("Ended(Closed)").actions, NO_ACTIONS);
    assert_eq!(sim.machine.phase(), Phase::Stopped);
    assert!(!sim.state.connected);
    let state = sim.state.clone();
    let steps = sim.steps.len();
    for input in [
        Input::Up(1),
        Input::Down(0),
        Input::Gone(0),
        Input::BluezLost,
        Input::Packet(pods_samples::handshake_ack()),
        Input::Tick,
        Input::Wait(ms(600_000)),
        Input::CallFailed,
        Input::Shutdown,
    ] {
        sim.feed(input);
    }
    assert!(
        sim.steps[steps..]
            .iter()
            .all(|step| step.actions.is_empty())
    );
    assert_eq!(sim.state, state);
    assert_eq!(sim.machine.deadline(), None);
    sim.feed(Input::Command(Command::SetEarDetection(
        EarDetectionBehavior::Disabled,
    )));
    assert_eq!(sim.last(), ["Resolve(0)"]);
    assert_eq!(
        sim.outcomes(),
        [Outcome::Refused(CommandError::NotConnected)]
    );
    assert_eq!(sim.state, state);
    sim.check_all_tickets_resolved_once();

    let mut sim = Sim::answering(Answer::Refuse);
    sim.feed(Input::Up(0));
    assert!(matches!(sim.machine.phase(), Phase::Backoff { .. }));
    sim.feed(Input::Shutdown);
    assert_eq!(sim.last(), NO_ACTIONS);
    assert_eq!(sim.machine.phase(), Phase::Stopped);
    sim.run_until(ms(600_000));
    assert_eq!(sim.times_of("Open(0)"), [0]);
    sim.check();
}

/// Each statement of the C++ `DeviceInfo::reset`, with the `State` fields it clears.
const CPP_RESET: [(&str, &str); 8] = [
    ("setDeviceName(\"\");", "device_name"),
    ("setModel(AirPodsModel::Unknown);", "model"),
    ("m_battery->reset();", "batteries, primary"),
    ("setBatteryStatus(\"\");", "batteries"),
    ("setNoiseControlMode(NoiseControlMode::Off);", "noise_mode"),
    (
        "setBluetoothAddress(\"\");",
        "the address lives in the link machine",
    ),
    ("getEarDetection()->reset();", "ear"),
    ("setHearingAidEnabled(false);", "not in the state"),
];

#[test]
fn reset_device_matches_the_cpp_reset() {
    let statements: Vec<&str> = DEVICE_INFO
        .body("void reset()")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let expected: Vec<&str> = CPP_RESET.iter().map(|(cpp, _)| *cpp).collect();
    assert_eq!(statements, expected);

    let mut state = State {
        connected: true,
        device_name: "Test AirPods".into(),
        model: Model::from_number("A3063"),
        model_number: "A3063".into(),
        noise_mode: NoiseMode::Adaptive,
        adaptive_noise_level: 20,
        conversational_awareness: true,
        one_bud_anc_mode: true,
        batteries: Batteries {
            left: Some(Charge {
                level: 80,
                charging: true,
            }),
            right: Some(Charge {
                level: 70,
                charging: false,
            }),
            case: Some(Charge {
                level: 60,
                charging: false,
            }),
            headset: None,
        },
        ear: EarReport {
            primary: EarPlacement::InEar,
            secondary: EarPlacement::InCase,
        },
        primary: Some(Component::Left),
        ear_detection_behavior: EarDetectionBehavior::Disabled,
        lid_state: LidState::Open,
        counters: crate::state::Counters {
            reconnect_failures: 3,
            ..Default::default()
        },
    };
    let kept = state.clone();
    state.reset_device();
    let defaults = State::default();
    assert_eq!(
        state,
        State {
            connected: false,
            device_name: defaults.device_name,
            model: Model::Unknown,
            noise_mode: NoiseMode::Off,
            batteries: defaults.batteries,
            ear: defaults.ear,
            primary: None,
            ..kept
        }
    );
}
