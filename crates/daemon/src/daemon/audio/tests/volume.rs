//! Volume writes at the audio adapter's request queue and socket.

use std::ffi::CString;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use pods_core::policy::audio::{AudioFact, AudioRequest, Percent, VolumeStamp};
use pods_core::state::State;
use pulseaudio::protocol::{
    Command, CommandTag, PulseError, SetDeviceVolumeParams, SinkState, SubscriptionEvent,
    SubscriptionEventFacility, SubscriptionEventType, SubscriptionMask, write_command_message,
    write_error,
};
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

use super::super::volume::{Observed, VolumeWrite, Volumes, device_sink, protocol_volume};
use super::super::wire::PROTOCOL_VERSION;
use super::super::wire::tests::{sink, sink_list};
use super::super::{Connection, Follower, Input, Left, Requests, Restore, queue};
use super::restore::{ack, answer, read_command, reply};
use super::{DEVICE, HALF, NORM, OUTPUT};
use crate::daemon::paused::on_paused_clock;

const OLD_INDEX: u32 = 7;
const NEW_INDEX: u32 = 15;
const FACT_QUEUE: usize = 4;
const EVENT_SEQ: u32 = u32::MAX;

fn set_volume(percent: Percent, writes: u64) -> AudioRequest {
    let stamp = VolumeStamp { epoch: 0, writes };
    AudioRequest::SetVolume(percent, stamp)
}

#[test]
fn the_audio_follower_stays_within_its_main_line_count() {
    const MAIN_LINES: usize = 832;
    assert!(include_str!("../../audio.rs").lines().count() <= MAIN_LINES);
}

#[test]
fn a_pending_volume_write_holds_the_queue_end() {
    let (sender, mut receiver) = queue();
    sender.try_send(set_volume(Percent::FULL, 1)).unwrap();
    drop(sender);
    let mut requests = Requests {
        queue: &mut receiver,
        restore: Restore::Clear,
        volume: None,
    };
    let mut context = Context::from_waker(Waker::noop());
    assert_eq!(requests.poll_next(&mut context), Poll::Pending);
    assert_eq!(requests.poll_next(&mut context), Poll::Pending);
    let write = requests.volume.take().unwrap();
    assert_eq!(write.lowered(), Some(Percent::FULL));
    assert!(!write.retried);
    assert_eq!(requests.poll_next(&mut context), Poll::Ready(None));
}

#[test]
fn set_volume_rounds_to_the_nearest_protocol_value() {
    on_paused_clock(async {
        // At full scale 65536, one percent rounds down to 655 and two percent rounds up to 1311.
        let cases = [(1, 655), (2, 1311)];
        let (stream, mut server) = UnixStream::pair().unwrap();
        let mut connection = Connection {
            stream,
            seq: 0,
            pending: Vec::new(),
        };
        let replies = tokio::spawn(async move {
            let mut volumes = Vec::new();
            for _ in cases {
                let (seq, params) = volume_command(&mut server).await;
                let [volume] = params.volume.channels() else {
                    panic!("expected one volume for all channels");
                };
                volumes.push(volume.as_u32());
                server.write_all(&ack(seq)).await.unwrap();
            }
            volumes
        });
        let mut stale = SubscriptionMask::empty();
        for (percent, _) in cases {
            let mut channels = pulseaudio::protocol::ChannelVolume::empty();
            channels.push(protocol_volume(Percent::new(percent).unwrap()));
            connection
                .set_channel_volume(OLD_INDEX, channels, &mut stale)
                .await
                .unwrap();
        }
        for ((_, expected), volume) in cases.into_iter().zip(replies.await.unwrap()) {
            assert_eq!(volume, expected);
        }
    });
}

#[test]
fn a_duck_dropped_on_a_non_default_sink_acknowledges_the_write() {
    on_paused_clock(async {
        let (stream, mut server) = UnixStream::pair().unwrap();
        let mut connection = Connection {
            stream,
            seq: 0,
            pending: Vec::new(),
        };
        let name = CString::new(OUTPUT).unwrap();
        let list = sink_list(&[(&name, &[NORM])]);
        let speakers_are_default = device_sink(&list, OUTPUT, None).unwrap();
        let mut volume = Volumes::default();
        let mut pending = VolumeWrite::queued(set_volume(Percent::new(20).unwrap(), 1));
        let observed = volume
            .apply(
                DEVICE.parse().unwrap(),
                &mut pending,
                &mut connection,
                speakers_are_default,
                &mut SubscriptionMask::empty(),
            )
            .await
            .unwrap();

        assert_eq!(volume.stamp.writes, 1, "the dropped duck must settle");
        assert!(matches!(observed, Observed::Skipped(stamp) if stamp == volume.stamp));
        assert!(pending.is_none());
        assert!(volume.saved.is_empty());

        let manual = protocol_volume(Percent::new(20).unwrap()).as_u32();
        let list = sink_list(&[(&name, &[manual])]);
        let manual = device_sink(&list, OUTPUT, None).unwrap();
        pending = VolumeWrite::queued(AudioRequest::RestoreVolume(
            Percent::FULL,
            VolumeStamp {
                epoch: 0,
                writes: 2,
            },
        ));
        let observed = volume
            .apply(
                DEVICE.parse().unwrap(),
                &mut pending,
                &mut connection,
                manual,
                &mut SubscriptionMask::empty(),
            )
            .await
            .unwrap();
        assert!(matches!(observed, Observed::Skipped(stamp) if stamp == volume.stamp));
        assert_no_volume_write(&mut server).await;
    });
}

#[test]
fn a_dropped_duck_acknowledges_the_write_after_a_finished_restore() {
    on_paused_clock(async {
        let (stream, mut server) = UnixStream::pair().unwrap();
        let mut connection = Connection {
            stream,
            seq: 0,
            pending: Vec::new(),
        };
        let replies = tokio::spawn(async move {
            for _ in 0..2 {
                let (seq, _) = volume_command(&mut server).await;
                server.write_all(&ack(seq)).await.unwrap();
            }
            server
        });
        let name = CString::new(OUTPUT).unwrap();
        let original = sink_list(&[(&name, &[NORM])]);
        let lowered = Percent::new(20).unwrap();
        let ducked = sink_list(&[(&name, &[protocol_volume(lowered).as_u32()])]);
        let device = DEVICE.parse().unwrap();
        let mut volume = Volumes::default();
        let mut stale = SubscriptionMask::empty();

        let mut pending = VolumeWrite::queued(set_volume(lowered, 1));
        volume
            .apply(
                device,
                &mut pending,
                &mut connection,
                device_sink(&original, OUTPUT, Some(OUTPUT)).unwrap(),
                &mut stale,
            )
            .await
            .unwrap();
        pending = VolumeWrite::queued(AudioRequest::RestoreVolume(
            Percent::FULL,
            VolumeStamp {
                epoch: 0,
                writes: 2,
            },
        ));
        volume
            .apply(
                device,
                &mut pending,
                &mut connection,
                device_sink(&ducked, OUTPUT, Some(OUTPUT)).unwrap(),
                &mut stale,
            )
            .await
            .unwrap();

        let mut server = replies.await.unwrap();
        assert_eq!(volume.saved.len(), 1);
        let stamp = VolumeStamp {
            epoch: 0,
            writes: 3,
        };
        pending = VolumeWrite::queued(AudioRequest::SetVolume(lowered, stamp));
        let observed = volume
            .apply(
                device,
                &mut pending,
                &mut connection,
                device_sink(&original, OUTPUT, None).unwrap(),
                &mut stale,
            )
            .await
            .unwrap();

        assert!(matches!(observed, Observed::Skipped(actual) if actual == stamp));
        assert!(volume.saved.is_empty());
        assert_no_volume_write(&mut server).await;
    });
}

#[test]
fn a_duck_replacing_a_queued_restore_keeps_the_original_channels() {
    on_paused_clock(async {
        let (stream, mut server) = UnixStream::pair().unwrap();
        let mut connection = Connection {
            stream,
            seq: 0,
            pending: Vec::new(),
        };
        let original = [NORM, HALF];
        let lowered = Percent::new(15).unwrap();
        let raw_lowered = protocol_volume(lowered).as_u32();
        let replies = tokio::spawn(async move {
            for expected in [[raw_lowered; 2], [raw_lowered; 2], original] {
                let (seq, params) = volume_command(&mut server).await;
                let actual: Vec<_> = params
                    .volume
                    .channels()
                    .iter()
                    .map(pulseaudio::protocol::Volume::as_u32)
                    .collect();
                assert_eq!(actual, expected);
                server.write_all(&ack(seq)).await.unwrap();
            }
        });
        let name = CString::new(OUTPUT).unwrap();
        let read = |channels: &[u32]| {
            let list = sink_list(&[(&name, channels)]);
            device_sink(&list, OUTPUT, Some(OUTPUT)).unwrap()
        };
        let original = read(&original).unwrap();
        let ducked = read(&[raw_lowered; 2]);
        let device = DEVICE.parse().unwrap();
        let mut volume = Volumes::default();
        let mut stale = SubscriptionMask::empty();
        let mut pending = VolumeWrite::queued(set_volume(lowered, 1));
        volume
            .apply(
                device,
                &mut pending,
                &mut connection,
                Some(original),
                &mut stale,
            )
            .await
            .unwrap();

        let restore = |writes| {
            AudioRequest::RestoreVolume(original.fact.volume, VolumeStamp { epoch: 0, writes })
        };
        pending = VolumeWrite::queued(restore(2));
        assert!(
            pending
                .replace(VolumeWrite::queued(set_volume(lowered, 3)).unwrap())
                .is_some()
        );
        volume
            .apply(device, &mut pending, &mut connection, ducked, &mut stale)
            .await
            .unwrap();

        pending = VolumeWrite::queued(restore(4));
        volume
            .apply(device, &mut pending, &mut connection, ducked, &mut stale)
            .await
            .unwrap();
        replies.await.unwrap();
    });
}

#[test]
fn a_replacement_during_a_refresh_drops_the_old_duck() {
    on_paused_clock(check_new_sink(RequestDuring::Refresh));
}

#[test]
fn a_replacement_found_by_a_volume_wake_drops_the_old_duck() {
    on_paused_clock(check_new_sink(RequestDuring::Wait));
}

#[test]
fn a_volume_wake_keeps_a_card_change_read_during_a_call() {
    const CARD_INDEX: u32 = 11;
    const CARD_REFRESH_WAIT: Duration = Duration::from_secs(1);

    on_paused_clock(async {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("pulse");
        let listener = UnixListener::bind(&socket).unwrap();
        let (inputs, mut facts) = mpsc::channel(FACT_QUEUE);
        let (requests, mut queue) = queue();
        let state = watch::channel(State::default()).1;
        let device = DEVICE.parse().unwrap();
        let mut follower = Follower::new(&inputs, &mut queue, device, state);
        let check = tokio::spawn(async move {
            let mut server = open_with_sink(&listener, &mut facts).await;
            notify_change(&mut server, SubscriptionEventFacility::Card, CARD_INDEX).await;
            let (seq, _) = expect_command(&mut server, CommandTag::GetCardInfoList).await;

            // A card read holds the connection while the volume request and the next card change arrive.
            requests.try_send(set_volume(Percent::FULL, 1)).unwrap();
            notify_change(&mut server, SubscriptionEventFacility::Card, CARD_INDEX).await;
            server.write_all(&ack(seq)).await.unwrap();

            let (seq, params) = refreshed_volume_command(&mut server, NORM).await;
            assert_eq!(params.device_index, Some(OLD_INDEX));
            server.write_all(&ack(seq)).await.unwrap();
            timeout(
                CARD_REFRESH_WAIT,
                answer_command(&mut server, CommandTag::GetCardInfoList),
            )
            .await
            .expect("the volume wake must refresh the queued card change");
        });
        assert!(matches!(
            follower.follow(&socket).await,
            Left::Request(None)
        ));
        check.await.unwrap();
    });
}

#[test]
fn a_missing_sink_retry_does_not_duck_the_replacement() {
    on_paused_clock(async {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("pulse");
        let listener = UnixListener::bind(&socket).unwrap();
        let (inputs, mut facts) = mpsc::channel(FACT_QUEUE);
        let (requests, mut queue) = queue();
        let state = watch::channel(State::default()).1;
        let device = DEVICE.parse().unwrap();
        let mut follower = Follower::new(&inputs, &mut queue, device, state);
        let check = tokio::spawn(async move {
            let mut server = open_with_sink(&listener, &mut facts).await;
            requests.try_send(set_volume(Percent::FULL, 1)).unwrap();
            answer_command(&mut server, CommandTag::GetServerInfo).await;
            answer_command_with_sink(&mut server, OLD_INDEX).await;
            let (seq, original) = volume_command(&mut server).await;
            assert_eq!(original.device_index, Some(OLD_INDEX));
            refuse_volume(&mut server, seq, &PulseError::NoEntity).await;

            answer_command(&mut server, CommandTag::GetServerInfo).await;
            answer_command_with_sink(&mut server, NEW_INDEX).await;
            assert_no_volume_write(&mut server).await;
            let Some(Input::Audio(AudioFact::Sink(Some(current)))) = facts.recv().await else {
                panic!("the next fact should report the replacement sink");
            };
            assert_eq!(current.index, NEW_INDEX);
            assert_eq!(
                current.volume_stamp,
                VolumeStamp {
                    epoch: 1,
                    writes: 0
                }
            );
        });
        assert!(matches!(
            follower.follow(&socket).await,
            Left::Request(None)
        ));
        check.await.unwrap();
    });
}

#[test]
fn two_missing_sink_refusals_drop_only_the_pending_write() {
    on_paused_clock(async {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("pulse");
        let listener = UnixListener::bind(&socket).unwrap();
        let (inputs, mut facts) = mpsc::channel(FACT_QUEUE);
        let (requests, mut queue) = queue();
        let state = watch::channel(State::default()).1;
        let device = DEVICE.parse().unwrap();
        let mut follower = Follower::new(&inputs, &mut queue, device, state);
        let check = tokio::spawn(async move {
            let mut server = open_with_sink(&listener, &mut facts).await;
            requests.try_send(set_volume(Percent::FULL, 1)).unwrap();
            for _ in 0..2 {
                let (seq, params) = refreshed_volume_command(&mut server, NORM).await;
                assert_eq!(params.device_index, Some(OLD_INDEX));
                assert_eq!(params.volume.channels().first().unwrap().as_u32(), NORM);
                refuse_volume(&mut server, seq, &PulseError::NoEntity).await;
            }

            requests
                .try_send(set_volume(Percent::new(50).unwrap(), 2))
                .unwrap();
            let (seq, next) = refreshed_volume_command(&mut server, HALF).await;
            assert_eq!(
                next.volume.channels().first().unwrap().as_u32(),
                HALF,
                "the refused write must yield to the new volume request"
            );
            refuse_volume(&mut server, seq, &PulseError::NoEntity).await;

            let (seq, retried) = refreshed_volume_command(&mut server, HALF).await;
            assert_eq!(retried.volume.channels(), next.volume.channels());
            refuse_volume(&mut server, seq, &PulseError::NoEntity).await;
            requests.try_send(AudioRequest::Follow(None)).unwrap();
            assert!(read_command(&mut server).await.is_none());
            let Some(Input::Audio(AudioFact::Sink(Some(sink)))) = facts.try_recv().ok() else {
                panic!("the refresh after the first dropped write reports its stamp");
            };
            assert_eq!(
                sink.volume_stamp,
                VolumeStamp {
                    epoch: 0,
                    writes: 1
                }
            );
            assert!(matches!(
                facts.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
        });
        let left = follower.follow(&socket).await;
        check.await.unwrap();
        assert!(matches!(
            left,
            Left::Request(Some(AudioRequest::Follow(None)))
        ));
    });
}

#[test]
fn a_volume_write_refused_for_another_reason_disconnects() {
    on_paused_clock(async {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("pulse");
        let listener = UnixListener::bind(&socket).unwrap();
        let (inputs, mut facts) = mpsc::channel(FACT_QUEUE);
        let (requests, mut queue) = queue();
        let state = watch::channel(State::default()).1;
        let device = DEVICE.parse().unwrap();
        let mut follower = Follower::new(&inputs, &mut queue, device, state);
        let check = tokio::spawn(async move {
            let mut server = open_with_sink(&listener, &mut facts).await;
            requests.try_send(set_volume(Percent::FULL, 1)).unwrap();
            let (seq, _) = refreshed_volume_command(&mut server, NORM).await;
            refuse_volume(&mut server, seq, &PulseError::Command).await;
            let next = read_command(&mut server)
                .await
                .map(|(_, command)| command.tag());
            assert_eq!(next, None, "the refusal must close this connection");
            assert!(matches!(
                facts.recv().await,
                Some(Input::Audio(AudioFact::Down))
            ));
            requests.try_send(AudioRequest::Follow(None)).unwrap();
        });
        let left = follower.follow(&socket).await;
        check.await.unwrap();
        assert!(matches!(
            left,
            Left::Request(Some(AudioRequest::Follow(None)))
        ));
    });
}

enum RequestDuring {
    Refresh,
    Wait,
}

async fn check_new_sink(during: RequestDuring) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pulse");
    let listener = UnixListener::bind(&socket).unwrap();
    let (inputs, mut facts) = mpsc::channel(FACT_QUEUE);
    let (requests, mut queue) = queue();
    let state = watch::channel(State::default()).1;
    let device = DEVICE.parse().unwrap();
    let mut follower = Follower::new(&inputs, &mut queue, device, state);
    let check = tokio::spawn(async move {
        let mut server = open_with_sink(&listener, &mut facts).await;

        let request_volume = || {
            requests.try_send(set_volume(Percent::FULL, 1)).unwrap();
        };
        match during {
            RequestDuring::Wait => {
                request_volume();
                answer_command(&mut server, CommandTag::GetServerInfo).await;
            }
            RequestDuring::Refresh => {
                notify_change(&mut server, SubscriptionEventFacility::Sink, OLD_INDEX).await;
            }
        }
        let (seq, _) = expect_command(&mut server, CommandTag::GetSinkInfoList).await;
        if matches!(during, RequestDuring::Refresh) {
            request_volume();
        }
        answer_sink_list(&mut server, seq, NEW_INDEX, HALF).await;
        assert_no_volume_write(&mut server).await;
        let Some(Input::Audio(AudioFact::Sink(Some(replacement)))) = facts.recv().await else {
            panic!("the next fact should report the replacement sink");
        };
        assert_eq!(replacement.index, NEW_INDEX);
        assert_eq!(replacement.volume.get(), 50);
        assert_eq!(
            replacement.volume_stamp,
            VolumeStamp {
                epoch: 1,
                writes: 0
            }
        );

        let restore_stamp = VolumeStamp {
            epoch: 1,
            writes: 1,
        };
        requests
            .try_send(AudioRequest::RestoreVolume(
                replacement.volume,
                restore_stamp,
            ))
            .unwrap();
        answer_command(&mut server, CommandTag::GetServerInfo).await;
        let (seq, _) = expect_command(&mut server, CommandTag::GetSinkInfoList).await;
        answer_sink_list(&mut server, seq, NEW_INDEX, HALF).await;
        assert_no_volume_write(&mut server).await;
        let fact = facts.recv().await;
        assert!(
            matches!(fact, Some(Input::Audio(AudioFact::VolumeSkipped(stamp))) if stamp == restore_stamp),
            "expected the skipped restore with {restore_stamp:?}, got {fact:?}"
        );

        requests
            .try_send(AudioRequest::SetVolume(
                Percent::new(10).unwrap(),
                VolumeStamp {
                    epoch: 1,
                    writes: 2,
                },
            ))
            .unwrap();
        answer_command(&mut server, CommandTag::GetServerInfo).await;
        let (seq, _) = expect_command(&mut server, CommandTag::GetSinkInfoList).await;
        answer_sink_list(&mut server, seq, NEW_INDEX, HALF).await;
        let (seq, params) = volume_command(&mut server).await;
        assert_eq!(params.device_index, Some(NEW_INDEX));
        assert_eq!(
            params.volume.channels(),
            &[protocol_volume(Percent::new(10).unwrap())]
        );
        server.write_all(&ack(seq)).await.unwrap();
    });
    assert!(matches!(
        follower.follow(&socket).await,
        Left::Request(None)
    ));
    check.await.unwrap();
}

async fn assert_no_volume_write(server: &mut UnixStream) {
    let unexpected = timeout(Duration::from_millis(1), read_command(server)).await;
    assert!(
        unexpected.is_err(),
        "unexpected command after the write was dropped: {:?}",
        unexpected.ok().flatten().map(|(_, command)| command.tag())
    );
}

async fn notify_change(server: &mut UnixStream, facility: SubscriptionEventFacility, index: u32) {
    let event = Command::SubscribeEvent(SubscriptionEvent {
        event_facility: facility,
        event_type: SubscriptionEventType::Changed,
        index: Some(index),
    });
    let mut bytes = Vec::new();
    write_command_message(&mut bytes, EVENT_SEQ, &event, PROTOCOL_VERSION).unwrap();
    server.write_all(&bytes).await.unwrap();
}

async fn open_with_sink(listener: &UnixListener, facts: &mut mpsc::Receiver<Input>) -> UnixStream {
    let (mut server, _) = listener.accept().await.unwrap();
    for tag in [
        CommandTag::Auth,
        CommandTag::SetClientName,
        CommandTag::Subscribe,
        CommandTag::GetServerInfo,
    ] {
        answer_command(&mut server, tag).await;
    }
    answer_command_with_sink(&mut server, OLD_INDEX).await;
    assert!(matches!(
        facts.recv().await,
        Some(Input::Audio(AudioFact::Up))
    ));
    answer_command(&mut server, CommandTag::GetCardInfoList).await;
    answer_command(&mut server, CommandTag::GetClientInfoList).await;
    let Some(Input::Audio(AudioFact::Sink(Some(first)))) = facts.recv().await else {
        panic!("the follower did not report the first sink");
    };
    assert_eq!(first.index, OLD_INDEX);
    assert!(matches!(
        facts.recv().await,
        Some(Input::Audio(AudioFact::Headset(false)))
    ));
    answer_command(&mut server, CommandTag::GetSourceInfoList).await;
    server
}

async fn answer_command(server: &mut UnixStream, expected: CommandTag) {
    let (seq, command) = expect_command(server, expected).await;
    let answer = answer(seq, &command, SinkState::Idle, None);
    server.write_all(&answer).await.unwrap();
}

async fn expect_command(server: &mut UnixStream, expected: CommandTag) -> (u32, Command) {
    let (seq, command) = read_command(server)
        .await
        .expect("the follower closed the connection before the next command");
    assert_eq!(command.tag(), expected);
    (seq, command)
}

async fn answer_command_with_sink(server: &mut UnixStream, index: u32) {
    let (seq, _) = expect_command(server, CommandTag::GetSinkInfoList).await;
    answer_sink_list(server, seq, index, NORM).await;
}

async fn answer_sink_list(server: &mut UnixStream, seq: u32, index: u32, volume: u32) {
    let list = vec![sink(index, &CString::new(OUTPUT).unwrap(), &[volume])];
    server.write_all(&reply(seq, &list)).await.unwrap();
}

async fn volume_command(server: &mut UnixStream) -> (u32, SetDeviceVolumeParams) {
    let (seq, command) = read_command(server).await.unwrap();
    let Command::SetSinkVolume(params) = command else {
        panic!("expected a volume write, heard {:?}", command.tag());
    };
    (seq, params)
}

async fn refreshed_volume_command(
    server: &mut UnixStream,
    volume: u32,
) -> (u32, SetDeviceVolumeParams) {
    answer_command(server, CommandTag::GetServerInfo).await;
    let (seq, _) = expect_command(server, CommandTag::GetSinkInfoList).await;
    answer_sink_list(server, seq, OLD_INDEX, volume).await;
    volume_command(server).await
}

async fn refuse_volume(server: &mut UnixStream, seq: u32, error: &PulseError) {
    let mut reply = Vec::new();
    write_error(&mut reply, seq, error).unwrap();
    server.write_all(&reply).await.unwrap();
}
