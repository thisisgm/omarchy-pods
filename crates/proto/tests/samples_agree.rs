//! Support tests: the sample builders agree with the protocol crate and the C++ sources.

// Inside cfg(test) so the lint table lets the tests unwrap.
#[cfg(test)]
mod tests {
    use pods_proto::battery::{Batteries, BatteryReport, Charge, Component};
    use pods_proto::control::{ControlCommand, ControlId};
    use pods_proto::ear::{EarPlacement, EarReport};
    use pods_proto::metadata::Metadata;
    use pods_proto::noise::NoiseMode;
    use pods_proto::packets::{HANDSHAKE, REQUEST_NOTIFICATIONS, SET_SPECIFIC_FEATURES};
    use pods_proto::uplink::UplinkPacket;
    use pods_samples::uplink::{STAMP_STEP, audio_sdu};
    use pods_samples::{
        ADAPTIVE_LEVEL, CONVERSATION_AWARENESS, Cell, NOISE, ONE_BUD_ANC, Placed, battery_of,
        ear_of, host, metadata_of, setting,
    };
    use proptest::prelude::*;

    /// Every manufacturer field the samples carry.
    const MANUFACTURER: &str = "Apple Inc.";
    const PLACEMENTS: [(Placed, EarPlacement); 3] = [
        (Placed::InEar, EarPlacement::InEar),
        (Placed::OutOfEar, EarPlacement::NotInEar),
        (Placed::InCase, EarPlacement::InCase),
    ];

    fn charge(cell: Cell) -> Charge {
        Charge {
            level: cell.level,
            charging: cell.charging,
        }
    }

    fn cell() -> impl Strategy<Value = Cell> {
        (0..=100_u8, any::<bool>()).prop_map(|(level, charging)| Cell { level, charging })
    }

    fn placement() -> impl Strategy<Value = (Placed, EarPlacement)> {
        prop::sample::select(PLACEMENTS.to_vec())
    }

    #[test]
    fn host_frames_equal_the_requests_the_daemon_sends() {
        for (frame, daemon, symbol) in [
            (host::handshake(), HANDSHAKE.to_vec(), "HANDSHAKE"),
            (
                host::set_specific_features(),
                SET_SPECIFIC_FEATURES.to_vec(),
                "SET_SPECIFIC_FEATURES",
            ),
            (
                host::request_notifications(),
                REQUEST_NOTIFICATIONS.to_vec(),
                "REQUEST_NOTIFICATIONS",
            ),
        ] {
            assert_eq!(frame, daemon, "{symbol}");
        }
    }

    #[test]
    fn a_setting_equals_the_command_the_daemon_builds() {
        assert_eq!(setting(NOISE, 2), NoiseMode::Anc.command().encode());
        for (identifier, id) in [
            (CONVERSATION_AWARENESS, ControlId::CONVERSATIONAL_AWARENESS),
            (ONE_BUD_ANC, ControlId::ONE_BUD_ANC),
        ] {
            assert_eq!(
                setting(identifier, 1),
                ControlCommand::switch(id, true).encode()
            );
            assert_eq!(
                setting(identifier, 2),
                ControlCommand::switch(id, false).encode()
            );
        }
        assert_eq!(ADAPTIVE_LEVEL, ControlId::ADAPTIVE_NOISE_LEVEL.0);
    }

    proptest! {
        #[test]
        fn parameterised_samples_parse_back_to_their_values(
            (left, right, case) in (cell(), cell(), cell()),
            (primary, secondary) in (placement(), placement()),
            (identifier, value) in (any::<u8>(), any::<u8>()),
            name in "[A-Za-z0-9 \u{2019}]{0,64}",
            model_number in "A[0-9]{4}",
        ) {
            let battery = BatteryReport::parse(&battery_of(left, right, case)).unwrap();
            prop_assert_eq!(battery, BatteryReport {
                batteries: Batteries {
                    left: Some(charge(left)),
                    right: Some(charge(right)),
                    case: Some(charge(case)),
                    headset: None,
                },
                primary: Some(Component::Right),
                secondary: Some(Component::Left),
            });
            let ear = EarReport::parse(&ear_of(primary.0, secondary.0)).unwrap();
            prop_assert_eq!(ear, EarReport { primary: primary.1, secondary: secondary.1 });
            let command = ControlCommand::parse(&setting(identifier, value)).unwrap();
            prop_assert_eq!(command, ControlCommand { id: ControlId(identifier), value: [value, 0, 0, 0] });
            let metadata = Metadata::parse(&metadata_of(&name, &model_number)).unwrap();
            prop_assert_eq!(metadata, Metadata { name, model_number, manufacturer: MANUFACTURER.to_owned() });
        }

        #[test]
        fn an_audio_sdu_parses_back_to_its_units_and_stamps(
            first_stamp in any::<u32>(),
            units in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..=255), 0..=8),
        ) {
            let sdu = audio_sdu(first_stamp, &units);
            let Ok(Some(UplinkPacket::Audio(audio))) = UplinkPacket::parse(&sdu) else {
                panic!("not an audio SDU: {sdu:02x?}");
            };
            let mut parsed = audio.units();
            let mut stamp = first_stamp;
            for unit in &units {
                let found = parsed.next().unwrap();
                prop_assert_eq!((found.stamp, found.data), (stamp, unit.as_slice()));
                stamp = stamp.wrapping_add(STAMP_STEP);
            }
            prop_assert_eq!((parsed.next(), parsed.trailing()), (None, 0));
        }
    }
}
