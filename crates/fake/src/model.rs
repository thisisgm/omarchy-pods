//! What the pods are and how they answer each frame, with no socket, thread or clock.

use std::collections::BTreeMap;

use pods_samples::{
    Cell, NOISE, Placed, battery_of, burst_others, burst_settings, ear_of, features_ack,
    handshake_ack, host, keys_of, metadata_of, setting,
};

use crate::{Fault, Profile};

/// The four bytes of the accessory header and the first byte of an opcode.
const FRAGMENT_LEN: usize = 5;

/// A change made by the test, as the stem or another host would make it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    Ear { primary: Placed, secondary: Placed },
    Battery { left: Cell, right: Cell, case: Cell },
    Setting { identifier: u8, value: u8 },
}

impl Change {
    /// The report a channel opened with `fault` gets for this change, if any.
    pub(crate) fn report(self, fault: Fault) -> Option<Vec<u8>> {
        let withheld = match fault {
            Fault::NeverAcknowledge => true,
            Fault::WithholdBattery => matches!(self, Self::Battery { .. }),
            Fault::None
            | Fault::CloseAfter(_)
            | Fault::ReportsBeforeAck
            | Fault::CutBattery
            | Fault::TrailingFragment
            | Fault::NeverConfirm => false,
        };
        (!withheld).then(|| self.packet())
    }

    // Layouts of daemon/airpods_packets.h, as `pods_samples` builds them.
    fn packet(self) -> Vec<u8> {
        match self {
            Self::Ear { primary, secondary } => ear_of(primary, secondary),
            Self::Battery { left, right, case } => battery_of(left, right, case),
            Self::Setting { identifier, value } => setting(identifier, value),
        }
    }
}

/// Whether a channel opened with `fault` closes once it has sent `sent` packets.
pub(crate) fn closes(fault: Fault, sent: usize) -> bool {
    matches!(fault, Fault::CloseAfter(limit) if sent >= limit)
}

#[derive(Debug)]
pub(crate) struct Model {
    metadata: Vec<u8>,
    keys: Option<Vec<u8>>,
    left: Cell,
    right: Cell,
    case: Cell,
    primary: Placed,
    secondary: Placed,
    settings: BTreeMap<u8, u8>,
}

impl Model {
    pub(crate) fn new(profile: Profile) -> Self {
        let Profile {
            name,
            model_number,
            left,
            right,
            case,
            primary,
            secondary,
            noise,
            keys,
        } = profile;
        Self {
            metadata: metadata_of(&name, &model_number),
            keys: keys.map(|(identity, payload)| keys_of(identity, payload)),
            left,
            right,
            case,
            primary,
            secondary,
            settings: BTreeMap::from([(NOISE, noise)]),
        }
    }

    /// What a channel opened with `fault` sends before the host sends anything.
    pub(crate) fn on_open(&self, fault: Fault) -> Vec<Vec<u8>> {
        match fault {
            Fault::ReportsBeforeAck => vec![
                battery_of(self.left, self.right, self.case),
                ear_of(self.primary, self.secondary),
            ],
            Fault::None
            | Fault::NeverAcknowledge
            | Fault::CloseAfter(_)
            | Fault::WithholdBattery
            | Fault::CutBattery
            | Fault::TrailingFragment
            | Fault::NeverConfirm => Vec::new(),
        }
    }

    /// The answer to one frame from the host on a channel opened with `fault`.
    pub(crate) fn on_host(&mut self, fault: Fault, sdu: &[u8]) -> Vec<Vec<u8>> {
        match fault {
            Fault::NeverAcknowledge | Fault::ReportsBeforeAck => return Vec::new(),
            Fault::None
            | Fault::CloseAfter(_)
            | Fault::WithholdBattery
            | Fault::CutBattery
            | Fault::TrailingFragment
            | Fault::NeverConfirm => {}
        }
        // Measured: each opening frame is answered in turn, the notification request by the burst.
        if sdu == host::handshake() {
            vec![handshake_ack()]
        } else if sdu == host::set_specific_features() {
            vec![features_ack()]
        } else if sdu == host::request_notifications() {
            self.burst(fault)
        } else if sdu == host::request_proximity_keys() {
            // corner: assumed until measured on hardware, the reply comes alone and at once
            self.keys.clone().into_iter().collect()
        } else if let Some((identifier, value)) = host_setting(sdu) {
            self.on_host_setting(fault, identifier, value)
        } else {
            Vec::new()
        }
    }

    pub(crate) fn apply(&mut self, change: Change) {
        match change {
            Change::Ear { primary, secondary } => {
                self.primary = primary;
                self.secondary = secondary;
            }
            Change::Battery { left, right, case } => {
                self.left = left;
                self.right = right;
                self.case = case;
            }
            Change::Setting { identifier, value } => {
                self.settings.insert(identifier, value);
            }
        }
    }

    fn on_host_setting(&mut self, fault: Fault, identifier: u8, value: u8) -> Vec<Vec<u8>> {
        if fault == Fault::NeverConfirm {
            return Vec::new();
        }
        // corner: assumed until measured on hardware, noise is ignored while neither bud is in an ear
        if identifier == NOISE && !self.either_bud_in_an_ear() {
            return Vec::new();
        }
        let change = Change::Setting { identifier, value };
        // corner: assumed until measured on hardware, a value the pods already hold is confirmed like a new one
        self.apply(change);
        // corner: assumed until measured on hardware, the pods confirm a setting with the same eleven bytes
        change.report(fault).into_iter().collect()
    }

    fn either_bud_in_an_ear(&self) -> bool {
        self.primary == Placed::InEar || self.secondary == Placed::InEar
    }

    // Opcode counts and the first two packets match a real device; the later order is made up, as in `pods_samples::first_burst`.
    fn burst(&self, fault: Fault) -> Vec<Vec<u8>> {
        let mut burst = Vec::new();
        if fault != Fault::WithholdBattery {
            let mut battery = battery_of(self.left, self.right, self.case);
            if fault == Fault::CutBattery {
                battery.pop();
            }
            burst.push(battery);
        }
        burst.push(ear_of(self.primary, self.secondary));
        burst.push(self.metadata.clone());
        let overrides: Vec<(u8, u8)> = self
            .settings
            .iter()
            .map(|(id, value)| (*id, *value))
            .collect();
        burst.extend(burst_settings(&overrides));
        burst.extend(burst_others());
        if fault == Fault::TrailingFragment {
            let mut fragment = battery_of(self.left, self.right, self.case);
            fragment.truncate(FRAGMENT_LEN);
            burst.push(fragment);
        }
        burst
    }
}

/// The identifier and value of a host frame equal to `setting`, as `0d 02` from `04 00 04 00 09 00 0d 02 00 00 00`.
fn host_setting(sdu: &[u8]) -> Option<(u8, u8)> {
    let &[_, _, _, _, _, _, identifier, value, _, _, _] = sdu else {
        return None;
    };
    (sdu == setting(identifier, value)).then_some((identifier, value))
}

#[cfg(test)]
mod tests {
    use std::slice;

    use pods_samples::{
        CONVERSATION_AWARENESS, IDENTITY_KEY, ONE_BUD_ANC, PAYLOAD_KEY, battery, ear, first_burst,
    };

    use super::*;

    const ALL_FAULTS: [Fault; 8] = [
        Fault::None,
        Fault::NeverAcknowledge,
        Fault::CloseAfter(3),
        Fault::ReportsBeforeAck,
        Fault::WithholdBattery,
        Fault::CutBattery,
        Fault::TrailingFragment,
        Fault::NeverConfirm,
    ];

    fn model() -> Model {
        Model::new(Profile::default())
    }

    fn out_of_ear() -> Model {
        Model::new(Profile {
            primary: Placed::OutOfEar,
            secondary: Placed::InCase,
            ..Profile::default()
        })
    }

    /// Setting `id` sits at `id + SETTING_OFFSET` in a burst: battery, ear and metadata come first, identifiers count from 1.
    const SETTING_OFFSET: usize = 2;

    fn noise_in_burst(model: &Model) -> Vec<u8> {
        model.burst(Fault::None)[usize::from(NOISE) + SETTING_OFFSET].clone()
    }

    #[test]
    fn the_opening_is_answered_frame_by_frame() {
        let mut model = model();
        assert_eq!(model.on_open(Fault::None), Vec::<Vec<u8>>::new());
        assert_eq!(
            model.on_host(Fault::None, &host::handshake()),
            [handshake_ack()]
        );
        assert_eq!(
            model.on_host(Fault::None, &host::set_specific_features()),
            [features_ack()]
        );
        let burst = model.on_host(Fault::None, &host::request_notifications());
        assert_eq!(burst, first_burst()[2..]);
    }

    #[test]
    fn the_burst_carries_the_profile() {
        let left = Cell {
            level: 10,
            charging: false,
        };
        let right = Cell {
            level: 20,
            charging: true,
        };
        let case = Cell {
            level: 30,
            charging: true,
        };
        let mut model = Model::new(Profile {
            name: "Synthetic Pods".to_owned(),
            model_number: "A0000".to_owned(),
            left,
            right,
            case,
            primary: Placed::OutOfEar,
            secondary: Placed::InCase,
            noise: 3,
            keys: None,
        });
        let mut expected = vec![
            battery_of(left, right, case),
            ear_of(Placed::OutOfEar, Placed::InCase),
            metadata_of("Synthetic Pods", "A0000"),
        ];
        expected.extend(burst_settings(&[(NOISE, 3)]));
        expected.extend(burst_others());
        assert_eq!(
            model.on_host(Fault::None, &host::request_notifications()),
            expected
        );
    }

    #[test]
    fn a_host_setting_is_applied_and_reported_back() {
        let mut model = model();
        let frame = setting(CONVERSATION_AWARENESS, 1);
        assert_eq!(
            model.on_host(Fault::None, &frame),
            [setting(CONVERSATION_AWARENESS, 1)]
        );
        assert_eq!(
            model.on_host(Fault::None, &setting(NOISE, 3)),
            [setting(NOISE, 3)]
        );
        assert_eq!(noise_in_burst(&model), setting(NOISE, 3));
    }

    #[test]
    fn an_unchanged_host_setting_is_reported_back_again() {
        let mut model = model();
        for _ in 0..2 {
            assert_eq!(
                model.on_host(Fault::None, &setting(NOISE, 1)),
                [setting(NOISE, 1)]
            );
        }
    }

    #[test]
    fn noise_is_ignored_with_no_bud_in_an_ear() {
        let mut model = out_of_ear();
        assert_eq!(
            model.on_host(Fault::None, &setting(NOISE, 2)),
            Vec::<Vec<u8>>::new()
        );
        assert_eq!(noise_in_burst(&model), setting(NOISE, 1));
        let other = setting(ONE_BUD_ANC, 2);
        assert_eq!(
            model.on_host(Fault::None, &other),
            [setting(ONE_BUD_ANC, 2)]
        );
        for (primary, secondary) in [
            (Placed::InEar, Placed::OutOfEar),
            (Placed::InCase, Placed::InEar),
        ] {
            let mut model = out_of_ear();
            model.apply(Change::Ear { primary, secondary });
            assert_eq!(
                model.on_host(Fault::None, &setting(NOISE, 2)),
                [setting(NOISE, 2)]
            );
        }
    }

    #[test]
    fn anything_else_is_answered_with_nothing() {
        let mut model = model();
        let mut tail_not_zero = setting(NOISE, 2);
        tail_not_zero[10] = 1;
        let mut handshake_longer = host::handshake();
        handshake_longer.push(0);
        let empty: Vec<Vec<u8>> = Vec::new();
        for frame in [
            Vec::new(),
            vec![0xde, 0xad],
            tail_not_zero,
            handshake_longer,
            setting(NOISE, 2)[..10].to_vec(),
            battery(),
        ] {
            assert_eq!(model.on_host(Fault::None, &frame), empty, "{frame:02x?}");
        }
        assert_eq!(noise_in_burst(&model), setting(NOISE, 1));
    }

    #[test]
    fn the_keys_request_is_answered_only_by_pods_with_keys() {
        let request = host::request_proximity_keys();
        assert_eq!(
            model().on_host(Fault::None, &request),
            Vec::<Vec<u8>>::new()
        );
        let mut keyed = Model::new(Profile {
            keys: Some((IDENTITY_KEY, PAYLOAD_KEY)),
            ..Profile::default()
        });
        let reply = keys_of(IDENTITY_KEY, PAYLOAD_KEY);
        for _ in 0..2 {
            assert_eq!(
                keyed.on_host(Fault::None, &request),
                slice::from_ref(&reply)
            );
        }
    }

    #[test]
    fn a_change_reports_its_new_value() {
        let mut model = model();
        let left = Cell {
            level: 5,
            charging: true,
        };
        let changes = [
            (
                Change::Ear {
                    primary: Placed::InCase,
                    secondary: Placed::OutOfEar,
                },
                ear_of(Placed::InCase, Placed::OutOfEar),
            ),
            (
                Change::Battery {
                    left,
                    right: left,
                    case: left,
                },
                battery_of(left, left, left),
            ),
            (
                Change::Setting {
                    identifier: NOISE,
                    value: 4,
                },
                setting(NOISE, 4),
            ),
        ];
        for (change, report) in changes {
            model.apply(change);
            assert_eq!(change.report(Fault::None), Some(report));
        }
        let burst = model.burst(Fault::None);
        assert_eq!(
            burst[..2],
            [
                battery_of(left, left, left),
                ear_of(Placed::InCase, Placed::OutOfEar)
            ]
        );
        assert_eq!(noise_in_burst(&model), setting(NOISE, 4));
    }

    #[test]
    fn never_acknowledge_answers_nothing() {
        let mut model = model();
        let fault = Fault::NeverAcknowledge;
        assert_eq!(model.on_open(fault), Vec::<Vec<u8>>::new());
        for frame in [
            host::handshake(),
            host::set_specific_features(),
            host::request_notifications(),
            setting(CONVERSATION_AWARENESS, 2),
        ] {
            assert_eq!(model.on_host(fault, &frame), Vec::<Vec<u8>>::new());
        }
        let change = Change::Setting {
            identifier: NOISE,
            value: 2,
        };
        assert_eq!(change.report(fault), None);
    }

    #[test]
    fn close_after_closes_at_its_count() {
        assert!(closes(Fault::CloseAfter(0), 0));
        assert!(!closes(Fault::CloseAfter(2), 1));
        assert!(closes(Fault::CloseAfter(2), 2));
        for fault in ALL_FAULTS {
            if !matches!(fault, Fault::CloseAfter(_)) {
                assert!(!closes(fault, usize::MAX), "{fault:?}");
            }
        }
    }

    #[test]
    fn reports_before_ack_opens_with_battery_and_ear_then_answers_nothing() {
        let mut model = model();
        let fault = Fault::ReportsBeforeAck;
        assert_eq!(model.on_open(fault), [battery(), ear()]);
        for frame in [host::handshake(), host::request_notifications()] {
            assert_eq!(model.on_host(fault, &frame), Vec::<Vec<u8>>::new());
        }
    }

    #[test]
    fn withhold_battery_drops_every_battery_packet_and_nothing_else() {
        let mut model = model();
        let fault = Fault::WithholdBattery;
        for _ in 0..2 {
            assert_eq!(
                model.on_host(fault, &host::request_notifications()),
                first_burst()[3..]
            );
        }
        let battery = Change::Battery {
            left: Cell {
                level: 1,
                charging: false,
            },
            right: Cell {
                level: 1,
                charging: false,
            },
            case: Cell {
                level: 1,
                charging: false,
            },
        };
        assert_eq!(battery.report(fault), None);
        let ear = Change::Ear {
            primary: Placed::InEar,
            secondary: Placed::InEar,
        };
        assert_eq!(
            ear.report(fault),
            Some(ear_of(Placed::InEar, Placed::InEar))
        );
    }

    #[test]
    fn never_confirm_ignores_host_settings_but_reports_the_stem() {
        let mut model = model();
        let fault = Fault::NeverConfirm;
        assert_eq!(
            model.on_host(fault, &setting(NOISE, 3)),
            Vec::<Vec<u8>>::new()
        );
        assert_eq!(noise_in_burst(&model), setting(NOISE, 1));
        assert_eq!(model.on_host(fault, &host::handshake()), [handshake_ack()]);
        let stem = Change::Setting {
            identifier: NOISE,
            value: 3,
        };
        assert_eq!(stem.report(fault), Some(setting(NOISE, 3)));
    }
}
