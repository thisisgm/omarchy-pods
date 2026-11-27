//! The requests each subscription event causes, against an audio server on a private socket.

use std::fs;
use std::time::Duration;

use crate::daemon::audio::wire::fixture::{
    Command, CommandTag, SinkInfo, SinkState, SourceOutputInfo, SubscriptionEvent,
    SubscriptionEventFacility, SubscriptionEventType, TagWriter, write_command_message,
    write_payload,
};
use pods_core::policy::audio::{AudioFact, AudioRequest, Recorders};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::super::super::streams::tests::stream_on;
use super::super::super::wire::tests::{read_request, sink as listed_sink};
use super::super::super::{Input, SOCKET, queue, run, stop};
use super::super::{DEVICE, HALF, NORM};
use super::{FACTS, MICROPHONE_INDEX, SINK_INDEX, answer, microphone, output, reply, source_up};
use crate::daemon::paused::on_paused_clock;

const VOLUME_CHANGES: usize = 10;
const EVENT_SEQUENCE: u32 = u32::MAX;
/// On the paused clock, an idle socket has had time to deliver the next immediate request.
const QUIET: Duration = Duration::from_millis(1);
const ALL_READS: [CommandTag; 6] = [
    CommandTag::GetServerInfo,
    CommandTag::GetSinkInfoList,
    CommandTag::GetCardInfoList,
    CommandTag::GetClientInfoList,
    CommandTag::GetSourceInfoList,
    CommandTag::GetSourceOutputInfoList,
];

struct AudioServer {
    stream: UnixStream,
    pending: Vec<u8>,
    facts: mpsc::Receiver<Input>,
    requests: mpsc::Sender<AudioRequest>,
    adapter: JoinHandle<()>,
    _dir: TempDir,
    volume: u32,
    recorders: Vec<SourceOutputInfo>,
    answered: Vec<CommandTag>,
}

impl AudioServer {
    async fn open() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join(SOCKET);
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(socket).unwrap();
        let (inputs, facts) = mpsc::channel(FACTS);
        let (requests, asked) = queue();
        let state = watch::channel(source_up()).1;
        let adapter = tokio::spawn(run(
            inputs,
            asked,
            dir.path().to_path_buf(),
            state,
            watch::channel(None).0,
        ));
        let device = DEVICE.parse().unwrap();
        requests
            .send(AudioRequest::Follow(Some(device)))
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut server = Self {
            stream,
            pending: Vec::new(),
            facts,
            requests,
            adapter,
            _dir: dir,
            volume: NORM,
            recorders: Vec::new(),
            answered: Vec::new(),
        };
        for expected in [
            CommandTag::Auth,
            CommandTag::SetClientName,
            CommandTag::Subscribe,
            CommandTag::GetServerInfo,
            CommandTag::GetSinkInfoList,
            CommandTag::GetCardInfoList,
            CommandTag::GetClientInfoList,
            CommandTag::GetSourceInfoList,
            CommandTag::SetDefaultSource,
            CommandTag::GetSourceOutputInfoList,
        ] {
            let (seq, command) = read_request(&mut server.stream, &mut server.pending).await;
            server.answer(seq, &command).await;
            assert_eq!(server.answered.last(), Some(&expected));
        }
        server.answered.clear();
        assert_eq!(server.fact().await, AudioFact::Up);
        assert!(matches!(server.fact().await, AudioFact::Sink(Some(_))));
        assert_eq!(server.fact().await, AudioFact::Headset(false));
        server
    }

    async fn answer(&mut self, seq: u32, command: &Command) {
        let state = SinkState::Running;
        let tag = command.tag();
        let bytes = if tag == CommandTag::GetSinkInfoList {
            let sink = SinkInfo {
                state,
                ..listed_sink(SINK_INDEX, &output(), &[self.volume])
            };
            reply(seq, &vec![sink])
        } else if tag == CommandTag::GetSourceOutputInfoList {
            reply(seq, &self.recorders)
        } else {
            answer(seq, command, state, Some(&microphone()))
        };
        self.stream.write_all(&bytes).await.unwrap();
        self.answered.push(tag);
    }

    async fn answer_until_quiet(&mut self) {
        while let Ok((seq, command)) =
            timeout(QUIET, read_request(&mut self.stream, &mut self.pending)).await
        {
            self.answer(seq, &command).await;
        }
    }

    async fn announce(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.unwrap();
        self.answer_until_quiet().await;
    }

    async fn fact(&mut self) -> AudioFact {
        let Some(Input::Audio(fact)) = self.facts.recv().await else {
            panic!("the audio adapter did not report a fact");
        };
        fact
    }

    async fn finish(self) -> Vec<CommandTag> {
        drop(self.requests);
        stop(self.adapter).await;
        self.answered
    }
}

fn event(
    facility: SubscriptionEventFacility,
    kind: SubscriptionEventType,
    index: Option<u32>,
) -> Vec<u8> {
    let command = Command::SubscribeEvent(SubscriptionEvent {
        event_facility: facility,
        event_type: kind,
        index,
    });
    let mut bytes = Vec::new();
    write_command_message(&mut bytes, EVENT_SEQUENCE, &command).unwrap();
    bytes
}

fn event_without_index() -> Vec<u8> {
    let mut payload = Vec::new();
    let mut tags = TagWriter(&mut payload);
    tags.u32(CommandTag::SubscribeEvent.number());
    tags.u32(EVENT_SEQUENCE);
    tags.u32(SubscriptionEventFacility::Sink.number() | SubscriptionEventType::Changed.number());
    let mut bytes = Vec::new();
    write_payload(&mut bytes, &payload).unwrap();
    bytes
}

#[test]
#[expect(
    clippy::print_stderr,
    reason = "reports the measured count on both builds"
)]
fn ten_sink_volume_events_read_only_the_sink_list() {
    let answered = on_paused_clock(async {
        let mut server = AudioServer::open().await;
        let changed = event(
            SubscriptionEventFacility::Sink,
            SubscriptionEventType::Changed,
            Some(SINK_INDEX),
        );
        for (volume, percent) in [(HALF, 50), (NORM, 100)]
            .into_iter()
            .cycle()
            .take(VOLUME_CHANGES)
        {
            server.volume = volume;
            server.announce(&changed).await;
            let AudioFact::Sink(Some(sink)) = server.fact().await else {
                panic!("the sink volume change was not reported");
            };
            assert_eq!(sink.volume.get(), percent);
        }
        server.finish().await
    });
    eprintln!("ten sink volume events: {} calls", answered.len());
    assert_eq!(answered, [CommandTag::GetSinkInfoList; VOLUME_CHANGES]);
}

#[test]
fn a_new_recorder_is_told_after_reading_only_the_record_streams() {
    let answered = on_paused_clock(async {
        let mut server = AudioServer::open().await;
        let recorder = stream_on(MICROPHONE_INDEX, None);
        let added = event(
            SubscriptionEventFacility::SourceOutput,
            SubscriptionEventType::New,
            Some(recorder.index),
        );
        server.recorders.push(recorder);
        server.announce(&added).await;
        assert_eq!(server.fact().await, AudioFact::Recording(true));
        assert_eq!(
            server.fact().await,
            AudioFact::Recorders(Recorders {
                dictation: 0,
                other: 1,
            })
        );
        server.finish().await
    });
    assert_eq!(answered, [CommandTag::GetSourceOutputInfoList]);
}

#[test]
fn a_malformed_event_reads_everything_and_the_next_server_event_reads_the_sinks() {
    on_paused_clock(async {
        let mut server = AudioServer::open().await;
        server.announce(&event_without_index()).await;
        assert_eq!(std::mem::take(&mut server.answered), ALL_READS);
        let changed = event(
            SubscriptionEventFacility::Server,
            SubscriptionEventType::Changed,
            None,
        );
        server.announce(&changed).await;
        assert_eq!(
            server.finish().await,
            [CommandTag::GetServerInfo, CommandTag::GetSinkInfoList]
        );
    });
}

#[test]
fn queued_sink_and_source_events_share_one_refresh() {
    on_paused_clock(async {
        let mut server = AudioServer::open().await;
        let sink = event(
            SubscriptionEventFacility::Sink,
            SubscriptionEventType::Changed,
            Some(SINK_INDEX),
        );
        let source = event(
            SubscriptionEventFacility::Source,
            SubscriptionEventType::Changed,
            Some(MICROPHONE_INDEX),
        );
        let queued = [sink.as_slice(), source.as_slice(), sink.as_slice()].concat();
        server.announce(&queued).await;
        assert_eq!(
            server.finish().await,
            [
                CommandTag::GetSinkInfoList,
                CommandTag::GetSourceInfoList,
                CommandTag::GetSourceOutputInfoList,
            ]
        );
    });
}

#[test]
fn events_read_during_a_call_refresh_only_the_changed_lists() {
    on_paused_clock(async {
        let mut server = AudioServer::open().await;
        let sink = event(
            SubscriptionEventFacility::Sink,
            SubscriptionEventType::Changed,
            Some(SINK_INDEX),
        );
        let source = event(
            SubscriptionEventFacility::Source,
            SubscriptionEventType::Changed,
            Some(MICROPHONE_INDEX),
        );

        server.stream.write_all(&sink).await.unwrap();
        let (seq, command) = read_request(&mut server.stream, &mut server.pending).await;
        assert_eq!(command.tag(), CommandTag::GetSinkInfoList);
        server.stream.write_all(&sink).await.unwrap();
        server.stream.write_all(&source).await.unwrap();
        server.answer(seq, &command).await;
        server.answer_until_quiet().await;

        assert_eq!(
            server.finish().await,
            [
                CommandTag::GetSinkInfoList,
                CommandTag::GetSinkInfoList,
                CommandTag::GetSourceInfoList,
                CommandTag::GetSourceOutputInfoList,
            ]
        );
    });
}
