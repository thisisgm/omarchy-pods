//! Support tests: the sample builders agree with the protocol crate and the C++ sources.

// Inside cfg(test) so the lint table lets the tests unwrap.
#[cfg(test)]
mod tests {
    use pods_proto::battery::{Batteries, BatteryReport, Charge, Component};
    use pods_proto::control::{ControlCommand, ControlId};
    use pods_proto::dictation::{DictationPacket, Event, Frame, encode_audio};
    use pods_proto::doap::{self, Handles, STOP_REASON_HOST, STOP_REASON_UNSOLICITED};
    use pods_proto::ear::{EarPlacement, EarReport};
    use pods_proto::metadata::Metadata;
    use pods_proto::noise::NoiseMode;
    use pods_proto::packets::{HANDSHAKE, REQUEST_NOTIFICATIONS, SET_SPECIFIC_FEATURES};
    use pods_proto::uplink::{self, UplinkPacket};
    use pods_samples::doap::{self as sample, host as sample_host};
    use pods_samples::uplink::{STAMP_STEP, START, STOP, audio_sdu};
    use pods_samples::{
        ADAPTIVE_LEVEL, CONVERSATION_AWARENESS, Cell, DOAP_MODE, NOISE, ONE_BUD_ANC, Placed,
        battery_of, ear_of, host, metadata_of, setting,
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
    fn the_dictation_samples_are_what_the_daemon_writes_and_reads() {
        let (ours, theirs) = (Handles::OBSERVED, sample::Handles::OBSERVED);
        let values = [
            (theirs.supported_codecs, ours.supported_codecs),
            (theirs.select_codec, ours.select_codec),
            (theirs.start, ours.start),
            (theirs.stop, ours.stop),
            (theirs.send_data, ours.send_data),
            (theirs.event, ours.event),
        ];
        for (sample, daemon) in values {
            assert_eq!(sample, daemon);
            assert_eq!(sample_host::read(sample), doap::read(daemon));
        }
        assert_eq!(theirs.cccds, ours.cccds);
        for descriptor in ours.cccds {
            assert_eq!(
                sample_host::subscribe(descriptor),
                doap::cccd_on(descriptor)
            );
        }
        assert_eq!(setting(DOAP_MODE, 1), doap::doap_mode_on());
        assert_eq!(
            sample_host::select_codec(&theirs),
            doap::select_codec(&ours)
        );
        assert_eq!(sample_host::start(&theirs), doap::start(&ours));
        for (sample, daemon) in [
            (sample::REASON_HOST, STOP_REASON_HOST),
            (sample::REASON_UNASKED, STOP_REASON_UNSOLICITED),
        ] {
            assert_eq!(
                sample_host::stop(&theirs, sample),
                doap::stop(&ours, daemon)
            );
            let stopped = DictationPacket::Stopped { reason: daemon };
            let notified = sample::stopped(&theirs, sample);
            assert_eq!(DictationPacket::parse(&notified, &ours), Ok(Some(stopped)));
        }
        for (notified, event) in [
            (sample::score(&theirs), Event::STARTED),
            (sample::end(&theirs), Event::StreamingEnd),
        ] {
            let read = DictationPacket::parse(&notified, &ours);
            assert_eq!(read, Ok(Some(DictationPacket::Event(event))));
        }
        let data: [&[u8]; 2] = [&[0xb8, 0x10, 0x11], &[0xb8]];
        let frames = [(0xffff_u16, data[0]), (0, data[1])];
        let coded = frames.map(|(counter, data)| Frame {
            flag: 0x80,
            level: 0xd5,
            counter,
            data,
        });
        let audio = sample::audio(&theirs, 0x2a, &frames);
        assert_eq!(encode_audio(&ours, 0x2a, &coded).unwrap(), audio);
        let Ok(Some(DictationPacket::Audio(read))) = DictationPacket::parse(&audio, &ours) else {
            panic!("not an audio notification: {audio:02x?}");
        };
        assert!(read.length_matches());
        assert_eq!(read.packet_counter, 0x2a);
        assert_eq!(read.frames().collect::<Vec<_>>(), coded);
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
    fn the_uplink_start_and_stop_equal_the_packets_the_protocol_crate_sends() {
        assert_eq!(START, uplink::START);
        assert_eq!(STOP, uplink::STOP);
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
