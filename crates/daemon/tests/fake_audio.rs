//! Support tests: the fake audio server, driven by a client built from the same protocol crate; no user path yet.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests drive the fake audio server alone, so the daemon helpers the other suites share go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::{CStr, CString};
    use std::io::{ErrorKind, Read, Write};
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    use proptest::collection::vec;
    use proptest::prelude::*;

    use pulseaudio::protocol::{
        AuthParams, AuthReply, CardInfo, ChannelVolume, Command, CommandReply, CommandTag,
        DEFAULT_SINK, DESCRIPTOR_SIZE, GetCardInfo, GetSinkInfo, MAX_VERSION, Prop, Props,
        ProtocolError, PulseError, ServerInfo, SetCardProfileParams, SetClientNameReply,
        SetDeviceVolumeParams, SinkInfo, SinkState, SourceInfo, SourceOutputInfo,
        SubscriptionEvent, SubscriptionEventFacility, SubscriptionEventType, SubscriptionMask,
        Volume, read_ack_message, read_command_message, read_descriptor, read_reply_message,
        write_command_message,
    };

    use super::support::Session;
    use super::support::audio::wire::{
        OVERSIZED_LENGTH, card_of, command, header, percent_of, read_frame, sink_of, volume_of,
    };
    use super::support::audio::{Asked, FakeAudio, FakeProfile, Recorder, Volumes};
    use super::support::rig::Rig;

    /// A reply or an event later than this is lost; the fake answers in microseconds.
    const ANSWER_WITHIN: Duration = Duration::from_secs(2);
    /// How long a held reply must stay unsent before the test releases it.
    const QUIET: Duration = Duration::from_millis(200);
    const ADDED_PERCENT: u8 = 40;
    const FIRST: &str = "02:00:00:00:00:01";
    const SECOND: &str = "02:00:00:00:00:02";
    const SINK_CHANNELS: usize = 2;
    /// The native protocol's authorisation cookie, which the fake does not check.
    const COOKIE_LENGTH: usize = 256;
    /// Bytes of a 20 byte descriptor sent before the close.
    const DESCRIPTOR_KEPT: usize = 5;
    /// One byte short of the 10 byte `GetServerInfo` payload.
    const PAYLOAD_KEPT: usize = 9;
    /// Longer than any command the fake answers, so every reader is driven past its last field.
    const ARBITRARY_MAX: usize = 96;
    const MICROPHONE: &str = "alsa_input.internal";
    /// A recorder may name a source that is not there, as one held on the pods' input while they are away.
    const ABSENT_SOURCE: &str = "bluez_input.02:00:00:00:00:02";
    /// What the server writes as the source of a recorder linked to none.
    const NO_SOURCE: u32 = u32::MAX;
    const OFF: &str = "off";
    const SBC: &str = "a2dp-sink-sbc";
    const SBC_XQ: &str = "a2dp-sink-sbc_xq";
    const AAC: &str = "a2dp-sink";
    const HEADSET: &str = "headset-head-unit";
    /// The pods' card as the server offers it, one profile unavailable.
    const PROFILES: [FakeProfile; 5] = [
        profile(SBC, true, 1, 0),
        profile(SBC_XQ, false, 1, 0),
        profile(AAC, true, 1, 0),
        profile(HEADSET, true, 1, 1),
        profile(OFF, true, 0, 0),
    ];

    const fn profile(name: &'static str, available: bool, sinks: u32, sources: u32) -> FakeProfile {
        FakeProfile {
            name,
            available,
            sinks,
            sources,
        }
    }

    /// One connection to the fake, speaking only through the crate's own messages.
    struct Client {
        stream: UnixStream,
        seq: u32,
    }

    impl Client {
        fn connect(socket: &Path) -> Self {
            let stream = UnixStream::connect(socket).unwrap();
            stream.set_read_timeout(Some(ANSWER_WITHIN)).unwrap();
            Self { stream, seq: 0 }
        }

        /// Connected, authorised and named, as every client of the audio server opens.
        fn open(socket: &Path) -> Self {
            let mut client = Self::connect(socket);
            let auth = Command::Auth(AuthParams {
                version: MAX_VERSION,
                cookie: vec![0; COOKIE_LENGTH],
                ..AuthParams::default()
            });
            let reply: AuthReply = client.ask(&auth);
            assert_eq!(reply.version, MAX_VERSION);
            let mut props = Props::new();
            props.set(Prop::ApplicationName, c"fake audio test");
            let _: SetClientNameReply = client.ask(&Command::SetClientName(props));
            client
        }

        fn send(&mut self, command: &Command) {
            self.seq = self.seq.wrapping_add(1);
            let mut bytes = Vec::new();
            write_command_message(&mut bytes, self.seq, command, MAX_VERSION).unwrap();
            self.stream.write_all(&bytes).unwrap();
        }

        fn frame(&mut self) -> Vec<u8> {
            read_frame(&mut self.stream).unwrap()
        }

        fn reply<R: CommandReply>(&mut self) -> R {
            let (seq, reply) =
                read_reply_message(&mut self.frame().as_slice(), MAX_VERSION).unwrap();
            assert_eq!(seq, self.seq, "a reply to another command");
            reply
        }

        fn ask<R: CommandReply>(&mut self, command: &Command) -> R {
            self.send(command);
            self.reply()
        }

        fn ack(&mut self, command: &Command) {
            self.send(command);
            let seq = read_ack_message(&mut self.frame().as_slice()).unwrap();
            assert_eq!(seq, self.seq, "an ack of another command");
        }

        fn refused(&mut self, command: &Command) -> PulseError {
            self.send(command);
            match read_ack_message(&mut self.frame().as_slice()) {
                Err(ProtocolError::ServerError(error)) => error,
                other => panic!("an error reply expected, got {other:?}"),
            }
        }

        fn subscribe(&mut self, mask: SubscriptionMask) {
            self.ack(&Command::Subscribe(mask));
        }

        fn event(&mut self) -> SubscriptionEvent {
            match read_command_message(&mut self.frame().as_slice(), MAX_VERSION).unwrap() {
                (u32::MAX, Command::SubscribeEvent(event)) => event,
                other => panic!("an event expected, got {other:?}"),
            }
        }

        fn nothing_for(&mut self, quiet: Duration) {
            self.stream.set_read_timeout(Some(quiet)).unwrap();
            let mut byte = [0];
            match self.stream.read(&mut byte) {
                Ok(0) => panic!("the fake closed the connection during a quiet wait"),
                Ok(_) => panic!("a frame arrived during a quiet wait"),
                Err(error) => assert!(
                    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
                    "{error}"
                ),
            }
            self.stream.set_read_timeout(Some(ANSWER_WITHIN)).unwrap();
        }

        fn closed(&mut self) -> bool {
            let mut byte = [0];
            matches!(self.stream.read(&mut byte), Ok(0))
        }
    }

    fn c_name(name: &str) -> CString {
        CString::new(name).unwrap()
    }

    fn sink_info(name: &CString) -> Command {
        Command::GetSinkInfo(GetSinkInfo {
            index: None,
            name: Some(name.clone()),
        })
    }

    fn volume_write(name: &str, volume: ChannelVolume) -> Command {
        Command::SetSinkVolume(SetDeviceVolumeParams {
            device_index: None,
            device_name: Some(c_name(name)),
            volume,
        })
    }

    fn stereo(volume: Volume) -> ChannelVolume {
        let mut channels = ChannelVolume::empty();
        for _ in 0..SINK_CHANNELS {
            channels.push(volume);
        }
        channels
    }

    fn event(facility: SubscriptionEventFacility, index: Option<u32>) -> SubscriptionEvent {
        SubscriptionEvent {
            event_facility: facility,
            event_type: SubscriptionEventType::Changed,
            index,
        }
    }

    /// The fake with one sink at `ADDED_PERCENT`, made the default.
    fn one_sink(session: &Session) -> (FakeAudio, String) {
        let audio = FakeAudio::start(&session.runtime_dir());
        let name = sink_of(FIRST);
        audio.add_sink(&name, ADDED_PERCENT);
        audio.set_default_sink(&name);
        (audio, name)
    }

    #[test]
    #[should_panic(expected = "fake audio: expected to remove one client at index 0")]
    fn removing_a_client_twice_fails() {
        let session = Session::new();
        let audio = FakeAudio::start(&session.runtime_dir());
        let index = audio.add_client(None);
        audio.remove_client(index);
        audio.remove_client(index);
    }

    #[test]
    fn the_default_sink_and_its_volume_are_read() {
        let mut rig = Rig::before_system_bus();
        let audio = rig.start_audio();
        let name = sink_of(FIRST);
        assert_eq!(name, "bluez_output.02_00_00_00_00_01.1");
        audio.add_sink(&name, ADDED_PERCENT);
        audio.set_default_sink(&name);
        let mut client = Client::open(audio.socket());

        let server: ServerInfo = client.ask(&Command::GetServerInfo);
        assert_eq!(server.default_sink_name, Some(c_name(&name)));
        let by_name: SinkInfo = client.ask(&sink_info(&c_name(&name)));
        let by_default: SinkInfo = client.ask(&sink_info(&DEFAULT_SINK.to_owned()));
        assert_eq!(by_name, by_default);
        assert_eq!(by_name.name, c_name(&name));
        assert_eq!(by_name.cvolume, stereo(volume_of(ADDED_PERCENT)));
        let absent = client.refused(&sink_info(&c_name(&sink_of(SECOND))));
        assert_eq!(absent, PulseError::NoEntity);

        let expected = [
            Asked::Auth {
                version: MAX_VERSION,
            },
            Asked::ClientName,
            Asked::ServerInfo,
            Asked::SinkInfo { name: name.clone() },
            Asked::SinkInfo {
                name: DEFAULT_SINK.to_str().unwrap().to_owned(),
            },
            Asked::SinkInfo {
                name: sink_of(SECOND),
            },
        ];
        assert_eq!(audio.requests(), expected);
        drop(client);
        drop(audio);
        rig.finish();
    }

    #[test]
    fn every_sink_is_listed_in_the_order_added() {
        let session = Session::new();
        let (audio, first) = one_sink(&session);
        // Named to sort before the first, so a list in name order is told apart from one in the order added.
        let second = "alsa_output.speakers";
        audio.add_sink(second, ADDED_PERCENT);
        let mut client = Client::open(audio.socket());
        let opened = audio.requests().len();

        let sinks: Vec<SinkInfo> = client.ask(&Command::GetSinkInfoList);
        let names: Vec<CString> = sinks.iter().map(|sink| sink.name.clone()).collect();
        assert_eq!(names, [c_name(&first), c_name(second)]);
        assert_eq!(sinks[1].cvolume, stereo(volume_of(ADDED_PERCENT)));
        audio.remove_sink(&first);
        audio.remove_sink(second);
        let none: Vec<SinkInfo> = client.ask(&Command::GetSinkInfoList);
        assert!(none.is_empty(), "{none:?}");
        assert_eq!(
            audio.requests()[opened..],
            [Asked::SinkList, Asked::SinkList]
        );
    }

    #[test]
    fn a_volume_write_is_stored_and_announced() {
        let session = Session::new();
        let (audio, name) = one_sink(&session);
        let mut watcher = Client::open(audio.socket());
        watcher.subscribe(SubscriptionMask::SINK);
        let mut writer = Client::open(audio.socket());
        let index = writer.ask::<SinkInfo>(&sink_info(&c_name(&name))).index;

        let changed = event(SubscriptionEventFacility::Sink, Some(index));
        writer.ack(&volume_write(&name, stereo(volume_of(70))));
        audio.wait_volume(&name, 70, ANSWER_WITHIN);
        assert_eq!(watcher.event(), changed);
        watcher.nothing_for(QUIET);
        let stored: SinkInfo = writer.ask(&sink_info(&c_name(&name)));
        assert_eq!(stored.cvolume, stereo(volume_of(70)));
        assert!(audio.requests().iter().any(|asked| {
            matches!(asked, Asked::Volume { name: sink, index: None, percent: 70, .. } if sink == &name)
        }));

        audio.set_volume(&name, 30);
        assert_eq!(audio.volume(&name), 30);
        assert_eq!(watcher.event(), changed);
        watcher.nothing_for(QUIET);

        let mut mono = ChannelVolume::empty();
        mono.push(volume_of(50));
        writer.ack(&volume_write(&name, mono));
        audio.wait_volume(&name, 50, ANSWER_WITHIN);
        assert_eq!(watcher.event(), changed);
        let spread: SinkInfo = writer.ask(&sink_info(&c_name(&name)));
        assert_eq!(spread.cvolume, stereo(volume_of(50)));

        let three_channels = volume_write(&name, ChannelVolume::norm(3));
        assert_eq!(writer.refused(&three_channels), PulseError::Invalid);
        assert_eq!(audio.volume(&name), 50);
        watcher.nothing_for(QUIET);
    }

    #[test]
    fn a_subscriber_hears_a_default_sink_change() {
        let session = Session::new();
        let (audio, first) = one_sink(&session);
        let mut watcher = Client::open(audio.socket());
        watcher.subscribe(SubscriptionMask::SERVER);
        let second = sink_of(SECOND);
        let changed = event(SubscriptionEventFacility::Server, None);

        audio.add_sink(&second, ADDED_PERCENT);
        audio.set_default_sink(&second);
        assert_eq!(watcher.event(), changed);
        watcher.nothing_for(QUIET);
        let server: ServerInfo = watcher.ask(&Command::GetServerInfo);
        assert_eq!(server.default_sink_name, Some(c_name(&second)));

        audio.remove_sink(&second);
        assert_eq!(watcher.event(), changed);
        watcher.nothing_for(QUIET);
        let server: ServerInfo = watcher.ask(&Command::GetServerInfo);
        assert_eq!(server.default_sink_name, Some(c_name(&first)));

        audio.remove_sink(&first);
        assert_eq!(watcher.event(), changed);
        watcher.nothing_for(QUIET);
        let server: ServerInfo = watcher.ask(&Command::GetServerInfo);
        assert_eq!(server.default_sink_name, None);
        assert!(audio.requests().contains(&Asked::Subscribe {
            mask: SubscriptionMask::SERVER
        }));
    }

    #[test]
    fn stop_closes_every_client_and_restart_accepts_again() {
        let session = Session::new();
        let (audio, name) = one_sink(&session);
        let mut first = Client::open(audio.socket());
        let mut second = Client::open(audio.socket());
        audio.wait_clients(2, ANSWER_WITHIN);
        audio.set_volume(&name, 60);

        audio.stop();
        assert!(first.closed(), "the first client is still open");
        assert!(second.closed(), "the second client is still open");
        assert_eq!(audio.clients(), 0);
        assert!(!audio.socket().exists(), "the socket outlived stop");

        audio.restart(Volumes::Kept);
        let mut client = Client::open(audio.socket());
        let kept: SinkInfo = client.ask(&sink_info(&c_name(&name)));
        assert_eq!(kept.cvolume, stereo(volume_of(60)));

        audio.restart(Volumes::Forgotten);
        assert!(client.closed(), "restart left a client open");
        let mut client = Client::open(audio.socket());
        let forgotten: SinkInfo = client.ask(&sink_info(&c_name(&name)));
        assert_eq!(forgotten.cvolume, stereo(volume_of(ADDED_PERCENT)));
    }

    #[test]
    fn held_replies_arrive_after_release() {
        let session = Session::new();
        let (audio, name) = one_sink(&session);
        let mut client = Client::open(audio.socket());
        let before = audio.requests().len();

        audio.hold_replies(true);
        client.send(&Command::GetServerInfo);
        let asked = audio.wait_requests(before + 1, ANSWER_WITHIN);
        assert_eq!(asked.last(), Some(&Asked::ServerInfo));
        client.nothing_for(QUIET);

        audio.hold_replies(false);
        let server: ServerInfo = client.reply();
        assert_eq!(server.default_sink_name, Some(c_name(&name)));
    }

    #[test]
    #[expect(
        clippy::as_conversions,
        reason = "the discriminant of a fieldless enum is the protocol's command number"
    )]
    fn an_unknown_command_gets_an_error_reply() {
        let session = Session::new();
        let (audio, name) = one_sink(&session);
        let mut client = Client::open(audio.socket());

        let error = client.refused(&Command::GetModuleInfoList);
        assert_eq!(error, PulseError::Command);
        assert_eq!(
            audio.requests().last(),
            Some(&Asked::Unknown {
                command: u32::from(CommandTag::GetModuleInfoList as u8)
            })
        );
        let server: ServerInfo = client.ask(&Command::GetServerInfo);
        assert_eq!(server.default_sink_name, Some(c_name(&name)));
    }

    #[test]
    fn an_oversized_frame_arrives_with_its_declared_length() {
        let session = Session::new();
        let (audio, _) = one_sink(&session);
        let mut client = Client::open(audio.socket());

        audio.send_oversized_frame();
        let mut header = [0; DESCRIPTOR_SIZE];
        client.stream.read_exact(&mut header).unwrap();
        let descriptor = read_descriptor(&mut header.as_slice()).unwrap();
        assert_eq!(descriptor.length, OVERSIZED_LENGTH);
    }

    #[test]
    fn a_stop_during_a_half_sent_frame_is_not_a_fault() {
        let session = Session::new();
        let (audio, _) = one_sink(&session);
        let mut client = Client::open(audio.socket());
        let mut bytes = Vec::new();
        write_command_message(&mut bytes, 1, &Command::GetServerInfo, MAX_VERSION).unwrap();
        let half = DESCRIPTOR_SIZE.saturating_add(PAYLOAD_KEPT);
        client.stream.write_all(&bytes[..half]).unwrap();

        audio.stop();
        assert!(client.closed(), "stop left the client open");
    }

    /// Sends the first `kept` bytes of a command frame, closes the client's write half, and waits for the fake to drop it.
    fn cut_frame(kept: usize) {
        let session = Session::new();
        let (audio, _) = one_sink(&session);
        let mut client = Client::open(audio.socket());
        let mut bytes = Vec::new();
        write_command_message(&mut bytes, 1, &Command::GetServerInfo, MAX_VERSION).unwrap();
        client.stream.write_all(&bytes[..kept]).unwrap();
        client.stream.shutdown(Shutdown::Write).unwrap();
        audio.wait_clients(0, ANSWER_WITHIN);
    }

    #[test]
    #[should_panic(expected = "a frame cut short")]
    fn a_frame_cut_short_in_its_descriptor_is_a_fault() {
        cut_frame(DESCRIPTOR_KEPT);
    }

    #[test]
    #[should_panic(expected = "a frame cut short")]
    fn a_frame_cut_short_in_its_payload_is_a_fault() {
        cut_frame(DESCRIPTOR_SIZE.saturating_add(PAYLOAD_KEPT));
    }

    #[test]
    fn percent_and_protocol_volume_convert_both_ways() {
        let norm = Volume::NORM.as_u32();
        for (percent, raw) in [
            (0, 0),
            (1, 655),
            (50, norm / 2),
            (99, 64_881),
            (100, norm),
            (150, norm + norm / 2),
        ] {
            assert_eq!(volume_of(percent).as_u32(), raw, "{percent} %");
            assert_eq!(percent_of(Volume::from_u32_clamped(raw)), percent, "{raw}");
        }
    }

    fn card_info(name: &str) -> Command {
        Command::GetCardInfo(GetCardInfo {
            index: None,
            name: Some(c_name(name)),
        })
    }

    fn profile_write(card: &str, profile: &str) -> Command {
        Command::SetCardProfile(SetCardProfileParams {
            card_index: None,
            card_name: Some(c_name(card)),
            profile_name: c_name(profile),
        })
    }

    /// The fake with the pods' card in `off`.
    fn one_card(session: &Session) -> (FakeAudio, String) {
        let audio = FakeAudio::start(&session.runtime_dir());
        let card = card_of(FIRST);
        audio.add_card(&card, &PROFILES, OFF);
        (audio, card)
    }

    /// A recorder as a test compares it: its source, whether it is corked, and every property as text.
    #[derive(Debug, PartialEq, Eq)]
    struct Listed {
        source: u32,
        corked: bool,
        props: BTreeMap<String, String>,
    }

    fn listed(info: &SourceOutputInfo) -> Listed {
        let props = info.props.iter().map(|(key, value)| {
            let value = CStr::from_bytes_with_nul(value).unwrap();
            (
                key.to_str().unwrap().to_owned(),
                value.to_str().unwrap().to_owned(),
            )
        });
        Listed {
            source: info.source_index,
            corked: info.corked,
            props: props.collect(),
        }
    }

    /// A recorder through the pulse server on `target`, with `extra` properties beside the four every one carries.
    fn expected(source: u32, target: &str, corked: bool, extra: &[(&str, &str)]) -> Listed {
        let corked_text = if corked { "true" } else { "false" };
        let mut props = BTreeMap::from([
            ("client.api", "pipewire-pulse"),
            ("media.class", "Stream/Input/Audio"),
            ("pulse.corked", corked_text),
            ("target.object", target),
        ]);
        props.extend(extra.iter().copied());
        let props = props
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()));
        Listed {
            source,
            corked,
            props: props.collect(),
        }
    }

    #[test]
    fn a_card_lists_its_profiles_and_its_active_one() {
        let session = Session::new();
        let audio = FakeAudio::start(&session.runtime_dir());
        let mut watcher = Client::open(audio.socket());
        watcher.subscribe(SubscriptionMask::CARD);
        let card = card_of(FIRST);
        assert_eq!(card, "bluez_card.02_00_00_00_00_01");
        audio.add_card(&card, &PROFILES, OFF);
        let mut client = Client::open(audio.socket());
        let opened = audio.requests().len();

        let cards: Vec<CardInfo> = client.ask(&Command::GetCardInfoList);
        assert_eq!(cards.len(), 1, "{cards:?}");
        let listed = &cards[0];
        let added = SubscriptionEvent {
            event_type: SubscriptionEventType::New,
            ..event(SubscriptionEventFacility::Card, Some(listed.index))
        };
        assert_eq!(watcher.event(), added);
        assert_eq!(listed.name, c_name(&card));
        assert_eq!(listed.active_profile, Some(c_name(OFF)));
        let profiles = listed.profiles.iter().map(|profile| {
            let counts = (profile.available, profile.num_sinks, profile.num_sources);
            (profile.name.clone(), counts)
        });
        let offered = PROFILES.iter().map(|profile| {
            let counts = (u32::from(profile.available), profile.sinks, profile.sources);
            (c_name(profile.name), counts)
        });
        assert!(profiles.eq(offered), "{:?}", listed.profiles);
        let by_name: CardInfo = client.ask(&card_info(&card));
        assert_eq!(&by_name, listed);
        let absent = client.refused(&card_info(&card_of(SECOND)));
        assert_eq!(absent, PulseError::NoEntity);
        assert_eq!(audio.active_profile(&card), OFF);
        assert_eq!(
            audio.requests()[opened..],
            [
                Asked::CardList,
                Asked::CardInfo { name: card.clone() },
                Asked::CardInfo {
                    name: card_of(SECOND)
                },
            ]
        );
        audio.assert_only_reads();
    }

    #[test]
    fn a_profile_write_is_stored_announced_and_recorded() {
        let session = Session::new();
        let (audio, card) = one_card(&session);
        let mut watcher = Client::open(audio.socket());
        watcher.subscribe(SubscriptionMask::CARD);
        let mut writer = Client::open(audio.socket());
        let index = writer.ask::<CardInfo>(&card_info(&card)).index;
        let changed = event(SubscriptionEventFacility::Card, Some(index));

        writer.ack(&profile_write(&card, SBC));
        assert_eq!(watcher.event(), changed);
        watcher.nothing_for(QUIET);
        assert_eq!(audio.active_profile(&card), SBC);
        let stored: CardInfo = writer.ask(&card_info(&card));
        assert_eq!(stored.active_profile, Some(c_name(SBC)));

        writer.ack(&profile_write(&card, SBC));
        watcher.nothing_for(QUIET);
        audio.set_profile_as_user(&card, AAC);
        assert_eq!(watcher.event(), changed);
        assert_eq!(audio.active_profile(&card), AAC);
        assert_eq!(audio.profile_writes(&card), [SBC, SBC]);
        assert!(audio.requests().contains(&Asked::Profile {
            card: card.clone(),
            profile: SBC.to_owned()
        }));
    }

    #[test]
    fn refused_writes_answer_an_error_the_given_number_of_times() {
        let session = Session::new();
        let (audio, card) = one_card(&session);
        let mut watcher = Client::open(audio.socket());
        watcher.subscribe(SubscriptionMask::CARD);
        let mut client = Client::open(audio.socket());
        let index = client.ask::<CardInfo>(&card_info(&card)).index;

        audio.refuse_profile_writes(2);
        let unknown = client.refused(&profile_write(&card, "a2dp-sink-unknown"));
        assert_eq!(unknown, PulseError::NoEntity);
        let absent = client.refused(&profile_write(&card_of(SECOND), AAC));
        assert_eq!(absent, PulseError::NoEntity);
        for _ in 0..2 {
            let refused = client.refused(&profile_write(&card, AAC));
            assert_eq!(refused, PulseError::AccessDenied);
            assert_eq!(audio.active_profile(&card), OFF);
        }
        watcher.nothing_for(QUIET);
        client.ack(&profile_write(&card, AAC));
        assert_eq!(
            watcher.event(),
            event(SubscriptionEventFacility::Card, Some(index))
        );
        assert_eq!(audio.active_profile(&card), AAC);
        assert_eq!(
            audio.profile_writes(&card),
            ["a2dp-sink-unknown", AAC, AAC, AAC]
        );
    }

    #[test]
    fn each_kind_of_recorder_is_listed_as_the_server_lists_it() {
        let session = Session::new();
        let (audio, sink) = one_sink(&session);
        audio.add_source(MICROPHONE);
        let mut watcher = Client::open(audio.socket());
        watcher.subscribe(SubscriptionMask::SOURCE_OUTPUT);
        let mut client = Client::open(audio.socket());
        let opened = audio.requests().len();
        let pods: SinkInfo = client.ask(&sink_info(&c_name(&sink)));
        let sources: Vec<SourceInfo> = client.ask(&Command::GetSourceInfoList);
        let names: Vec<CString> = sources.iter().map(|source| source.name.clone()).collect();
        assert_eq!(
            names,
            [c_name(&format!("{sink}.monitor")), c_name(MICROPHONE)]
        );
        let (monitor, microphone) = (&sources[0], &sources[1]);
        assert_eq!(monitor.monitor_of_sink_index, Some(pods.index));
        assert_eq!(pods.monitor_source_index, Some(monitor.index));
        assert_eq!(microphone.monitor_of_sink_index, None);

        let monitor_name = format!("{sink}.monitor");
        let kinds = [
            (MICROPHONE, Recorder::Program),
            (MICROPHONE, Recorder::PeakMeter),
            (sink.as_str(), Recorder::Monitor),
            (MICROPHONE, Recorder::Corked),
            (ABSENT_SOURCE, Recorder::Program),
            (monitor_name.as_str(), Recorder::PeakMeter),
        ];
        let mut ids = Vec::new();
        for (target, kind) in kinds {
            let id = audio.add_recorder(target, kind);
            let added = SubscriptionEvent {
                event_type: SubscriptionEventType::New,
                ..event(SubscriptionEventFacility::SourceOutput, Some(id.0))
            };
            assert_eq!(watcher.event(), added);
            ids.push(id);
        }
        let recorders: Vec<SourceOutputInfo> = client.ask(&Command::GetSourceOutputInfoList);
        let indexes: Vec<u32> = recorders.iter().map(|recorder| recorder.index).collect();
        assert_eq!(indexes, ids.iter().map(|id| id.0).collect::<Vec<u32>>());
        let shapes: Vec<Listed> = recorders.iter().map(listed).collect();
        let mic = microphone.index;
        assert_eq!(
            shapes,
            [
                expected(mic, MICROPHONE, false, &[]),
                expected(mic, MICROPHONE, false, &[("stream.monitor", "true")]),
                expected(
                    monitor.index,
                    &sink,
                    false,
                    &[("stream.capture.sink", "true")]
                ),
                expected(mic, MICROPHONE, true, &[]),
                expected(NO_SOURCE, ABSENT_SOURCE, false, &[]),
                expected(
                    monitor.index,
                    &monitor_name,
                    false,
                    &[("stream.monitor", "true")]
                ),
            ]
        );

        let peak = ids.remove(1);
        let index = peak.0;
        audio.remove_recorder(peak);
        let removed = SubscriptionEvent {
            event_type: SubscriptionEventType::Removed,
            ..event(SubscriptionEventFacility::SourceOutput, Some(index))
        };
        assert_eq!(watcher.event(), removed);
        let left: Vec<SourceOutputInfo> = client.ask(&Command::GetSourceOutputInfoList);
        let left: Vec<u32> = left.iter().map(|recorder| recorder.index).collect();
        assert_eq!(left, ids.iter().map(|id| id.0).collect::<Vec<u32>>());
        let lists = [Asked::SourceList, Asked::RecorderList, Asked::RecorderList];
        assert_eq!(audio.requests()[opened + 1..], lists);
        audio.assert_only_reads();
    }

    #[test]
    fn a_running_sink_reads_running_and_is_announced() {
        let session = Session::new();
        let (audio, name) = one_sink(&session);
        let mut watcher = Client::open(audio.socket());
        watcher.subscribe(SubscriptionMask::SINK);
        let index = watcher.ask::<SinkInfo>(&sink_info(&c_name(&name))).index;
        let changed = event(SubscriptionEventFacility::Sink, Some(index));

        for state in [SinkState::Running, SinkState::Idle] {
            audio.set_sink_state(&name, state);
            assert_eq!(watcher.event(), changed);
            let sink: SinkInfo = watcher.ask(&sink_info(&c_name(&name)));
            assert_eq!(sink.state, state);
            audio.set_sink_state(&name, state);
            watcher.nothing_for(QUIET);
        }
    }

    /// A descriptor that declares `payload` whole on the control channel, then the payload.
    fn framed(payload: &[u8]) -> Vec<u8> {
        let mut frame = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
        frame.extend([u8::MAX; 4]);
        frame.resize(DESCRIPTOR_SIZE, 0);
        frame.extend(payload);
        frame
    }

    /// Reads `bytes` as the fake does: a frame first, then its header and its command.
    fn read_as_the_fake_does(bytes: &[u8]) {
        if let Ok(frame) = read_frame(&mut &bytes[..]) {
            drop(header(&frame));
            drop(command(&frame, MAX_VERSION));
        }
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic_the_readers(bytes in vec(any::<u8>(), 0..ARBITRARY_MAX)) {
            read_as_the_fake_does(&bytes);
        }

        #[test]
        fn an_arbitrary_payload_in_a_whole_frame_never_panics_the_readers(
            payload in vec(any::<u8>(), 0..ARBITRARY_MAX),
        ) {
            let frame = framed(&payload);
            prop_assert_eq!(read_frame(&mut frame.as_slice()).ok(), Some(frame.clone()));
            read_as_the_fake_does(&frame);
        }
    }
}
