//! The driver's restore answers on a paused clock, against a server that holds the first restore's suspend until the policy has asked for a second.

use std::ffi::CString;
use std::fs;
use std::time::Duration;

use pods_core::link::Signal;
use pods_core::policy::audio::{AudioFact, AudioRequest, RestoreId};
use pods_core::policy::mic::{RESTORE_WAIT_MS, UplinkRequest};
use pods_core::policy::{Effect, Fact, Policy};
use pods_core::prefs::MicPref;
use pods_core::state::{Microphone, State};
use pods_proto::ear::EarReport;
use pods_proto::uplink::UplinkState;
use pods_samples::MODEL_NUMBER;
use pulseaudio::protocol::{
    AuthReply, Command, DESCRIPTOR_SIZE, PulseError, ServerInfo, SetClientNameReply, SinkInfo,
    SinkState, SuspendParams, read_descriptor, write_ack_message, write_error, write_reply_message,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use super::super::wire::PROTOCOL_VERSION;
use super::super::wire::tests::sink as listed;
use super::super::{Input, SOCKET, queue, run, stop};
use super::{DEVICE, NORM, OUTPUT};
use crate::daemon::paused::on_paused_clock;

const SINK_INDEX: u32 = 7;
/// Room for every fact the two restores tell.
const FACTS: usize = 8;

/// The real policy, stepped through two STOPs with its restore deadline between them.
struct Pods {
    policy: Policy,
    state: State,
    now: Duration,
    audio: Vec<AudioRequest>,
}

impl Pods {
    fn new() -> Self {
        let state = State {
            connected: true,
            model_number: MODEL_NUMBER.to_owned(),
            ear: EarReport::from_in_ear(true, true),
            ..State::default()
        };
        Self {
            policy: Policy::new(None),
            state,
            now: Duration::ZERO,
            audio: Vec::new(),
        }
    }

    fn step(&mut self, fact: Fact) -> Vec<Effect> {
        self.policy.apply(&mut self.state, &fact);
        let effects = self.policy.step(&self.state, fact, self.now);
        for effect in &effects {
            if let Effect::Audio(request) = effect {
                self.audio.push(*request);
            }
        }
        effects
    }

    /// Steps `fact` with the preference set, then each deadline in turn until `wanted` goes out.
    fn until(&mut self, preference: MicPref, fact: Fact, wanted: &Effect) {
        self.state.microphone = Microphone::new(preference);
        let mut effects = self.step(fact);
        while !effects.contains(wanted) {
            let now = self.now;
            match self.policy.deadline() {
                Some(deadline) if deadline > now => self.now = deadline,
                deadline => panic!("deadline {deadline:?} is not after {now:?} before {wanted:?}"),
            }
            effects = self.step(Fact::Tick);
        }
    }

    fn marker(&mut self, marker: UplinkState) {
        self.step(Fact::Link(Signal::Uplink(marker)));
    }

    fn start(&mut self) {
        self.until(
            MicPref::Always,
            Fact::Changed,
            &Effect::Uplink(UplinkRequest::Start),
        );
        self.marker(UplinkState::Running);
    }

    /// `off` until the STOP goes out, then its answer, which asks for the restore.
    fn stop(&mut self) {
        self.until(
            MicPref::Off,
            Fact::Changed,
            &Effect::Uplink(UplinkRequest::Stop),
        );
        self.marker(UplinkState::Stopped);
    }
}

/// The policy's audio requests: through its first restore, then what it sends after that restore's deadline.
fn two_restores() -> (Vec<AudioRequest>, Vec<AudioRequest>) {
    let mut pods = Pods::new();
    pods.start();
    pods.stop();
    let first = pods.audio.len();
    let asked = pods.now;
    pods.start();
    let restore_wait = Duration::from_millis(RESTORE_WAIT_MS);
    assert_eq!(pods.now, asked.saturating_add(restore_wait));
    pods.stop();
    let later = pods.audio.split_off(first);
    (pods.audio, later)
}

fn restore_ids(requests: &[AudioRequest]) -> Vec<RestoreId> {
    requests
        .iter()
        .filter_map(|request| match request {
            AudioRequest::Restore(id) => Some(*id),
            AudioRequest::Follow(_) | AudioRequest::CancelRestore => None,
        })
        .collect()
}

/// The server's half: every suspend in order, and the release of the first one's answer.
struct Server {
    suspends: mpsc::UnboundedReceiver<bool>,
    release: oneshot::Sender<()>,
}

/// Inside a paused runtime: one connection answered as a server would, the first suspend answered only on `release`.
fn serve(listener: UnixListener) -> Server {
    let (heard, suspends) = mpsc::unbounded_channel();
    let (release, released) = oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut released = Some(released);
        let mut state = SinkState::Running;
        loop {
            let Some((seq, command)) = read_command(&mut stream).await else {
                return;
            };
            if let Command::SuspendSink(SuspendParams { suspend, .. }) = command {
                heard.send(suspend).unwrap();
                if let Some(released) = released.take() {
                    released.await.unwrap();
                }
                state = if suspend {
                    SinkState::Suspended
                } else {
                    SinkState::Running
                };
                stream.write_all(&ack(seq)).await.unwrap();
                continue;
            }
            stream
                .write_all(&answer(seq, &command, state))
                .await
                .unwrap();
        }
    });
    Server { suspends, release }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "the server answers what the driver sends before a suspend and refuses every other command alike"
)]
fn answer(seq: u32, command: &Command, state: SinkState) -> Vec<u8> {
    match command {
        Command::Auth(params) => reply(
            seq,
            &AuthReply {
                version: params.version,
                ..AuthReply::default()
            },
        ),
        Command::SetClientName(_) => reply(seq, &SetClientNameReply { client_id: 0 }),
        Command::Subscribe(_) => ack(seq),
        Command::GetServerInfo => reply(seq, &server_info()),
        Command::GetSinkInfoList => reply(seq, &vec![sink(state)]),
        _ => error(seq),
    }
}

/// A frame as the driver writes it, sample: a 20 byte descriptor declaring 10 bytes, then a tagged command and sequence.
async fn read_command(stream: &mut UnixStream) -> Option<(u32, Command)> {
    let mut descriptor = [0; DESCRIPTOR_SIZE];
    // The driver closing the connection ends the server.
    stream.read_exact(&mut descriptor).await.ok()?;
    let length = read_descriptor(&mut descriptor.as_slice()).unwrap().length;
    let mut payload = vec![0; usize::try_from(length).unwrap()];
    stream.read_exact(&mut payload).await.unwrap();
    Some(Command::read_tag_prefixed(&mut payload.as_slice(), PROTOCOL_VERSION).unwrap())
}

fn reply(seq: u32, reply: &impl pulseaudio::protocol::CommandReply) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_reply_message(&mut bytes, seq, reply, PROTOCOL_VERSION).unwrap();
    bytes
}

fn ack(seq: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_ack_message(&mut bytes, seq).unwrap();
    bytes
}

fn error(seq: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_error(&mut bytes, seq, &PulseError::Command).unwrap();
    bytes
}

fn server_info() -> ServerInfo {
    ServerInfo {
        default_sink_name: Some(output()),
        ..ServerInfo::default()
    }
}

/// The followed device's sink, the one the restores suspend and resume.
fn output() -> CString {
    CString::new(OUTPUT).unwrap()
}

fn sink(state: SinkState) -> SinkInfo {
    SinkInfo {
        state,
        ..listed(SINK_INDEX, &output(), &[NORM])
    }
}

/// The next restore the adapter answers, past the sink facts before it.
async fn restored(facts: &mut mpsc::Receiver<Input>) -> RestoreId {
    loop {
        match facts.recv().await {
            Some(Input::Audio(AudioFact::Restored(id))) => return id,
            Some(_) => {}
            None => panic!("the adapter ended before a restore was answered"),
        }
    }
}

#[test]
fn a_restore_asked_inside_an_earlier_one_is_answered_after_it_with_its_own_id() {
    let (through_the_first, after_its_deadline) = two_restores();
    let [first] = restore_ids(&through_the_first)[..] else {
        panic!("{through_the_first:?}");
    };
    let [second] = restore_ids(&after_its_deadline)[..] else {
        panic!("{after_its_deadline:?}");
    };
    assert_ne!(first, second);
    let suspends = on_paused_clock(async {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join(SOCKET);
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let mut server = serve(UnixListener::bind(&socket).unwrap());
        let (inputs, mut facts) = mpsc::channel(FACTS);
        let (requests, asked) = queue();
        let adapter = tokio::spawn(run(inputs, asked, dir.path().to_path_buf()));
        let device = DEVICE.parse().unwrap();
        requests
            .send(AudioRequest::Follow(Some(device)))
            .await
            .unwrap();
        for request in through_the_first {
            requests.send(request).await.unwrap();
        }
        assert_eq!(server.suspends.recv().await, Some(true));
        for request in after_its_deadline {
            requests.send(request).await.unwrap();
        }
        server.release.send(()).unwrap();
        assert_eq!(restored(&mut facts).await, first);
        assert_eq!(restored(&mut facts).await, second);
        drop(requests);
        stop(adapter).await;
        let mut suspends = vec![true];
        while let Ok(suspend) = server.suspends.try_recv() {
            suspends.push(suspend);
        }
        suspends
    });
    assert_eq!(suspends, [true, false, true, false]);
}
