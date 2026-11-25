//! The driver's restore answers on a paused clock, against a server that holds one answer back until the test releases it.

mod events;

use std::ffi::{CStr, CString};
use std::fs;
use std::time::Duration;

use pods_core::link::Signal;
use pods_core::policy::audio::{AUDIO_CALL_TIMEOUT_MS, AudioFact, AudioRequest, RestoreId};
use pods_core::policy::mic::{RESTORE_WAIT_MS, UplinkRequest};
use pods_core::policy::{Effect, Fact, Policy};
use pods_core::prefs::MicPref;
use pods_core::state::{MicCode, Microphone, State};
use pods_proto::ear::EarReport;
use pods_proto::uplink::UplinkState;
use pods_samples::MODEL_NUMBER;
use pulseaudio::protocol::{
    AuthReply, Command, CommandTag, DESCRIPTOR_SIZE, PulseError, ServerInfo, SetClientNameReply,
    SinkInfo, SinkState, SourceOutputInfo, SuspendParams, read_descriptor, write_ack_message,
    write_error, write_reply_message,
};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;

use super::super::streams::tests::{MICROPHONE_INDEX, source};
use super::super::wire::PROTOCOL_VERSION;
use super::super::wire::tests::sink as listed;
use super::super::{Input, SOCKET, queue, run, stop};
use super::{DEVICE, NORM, OUTPUT};
use crate::daemon::paused::on_paused_clock;

const SINK_INDEX: u32 = 7;
/// Room for every fact the two restores tell.
const FACTS: usize = 8;
/// A source the server may list in place of the microphone's.
const ANOTHER_SOURCE: &CStr = c"alsa_input.another";
/// A device whose sink the server never lists.
const ANOTHER_DEVICE: &str = "02:00:00:00:00:01";

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
        self.state.ear = EarReport::from_in_ear(true, true);
        self.until(
            MicPref::Always,
            Fact::Changed,
            &Effect::Uplink(UplinkRequest::Start),
        );
        self.marker(UplinkState::Running);
    }

    /// `off` with no bud worn, so the gate lets the STOP out, then its answer, which asks for the restore.
    fn stop(&mut self) {
        self.state.ear = EarReport::default();
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

/// The server's half: every command heard and every suspend, each in order, and the release of the held answer.
struct Server {
    heard: mpsc::UnboundedReceiver<CommandTag>,
    suspends: mpsc::UnboundedReceiver<bool>,
    release: oneshot::Sender<()>,
}

/// Inside a paused runtime: each connection in turn answered as a server would, the sink's state kept between them and the first `held` command answered only on `release`; `lists` names the one source listed, and none refuses the list.
fn serve(listener: UnixListener, held: CommandTag, lists: Option<CString>) -> Server {
    let (commands, heard) = mpsc::unbounded_channel();
    let (suspended, suspends) = mpsc::unbounded_channel();
    let (release, released) = oneshot::channel();
    tokio::spawn(async move {
        let mut released = Some(released);
        let mut state = SinkState::Running;
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            while let Some((seq, command)) = read_command(&mut stream).await {
                commands.send(command.tag()).unwrap();
                if let Command::SuspendSink(SuspendParams { suspend, .. }) = command {
                    suspended.send(suspend).unwrap();
                    state = if suspend {
                        SinkState::Suspended
                    } else {
                        SinkState::Running
                    };
                }
                if command.tag() == held
                    && let Some(released) = released.take()
                {
                    released.await.unwrap();
                }
                let answer = answer(seq, &command, state, lists.as_deref());
                // The driver may have left this connection while its answer was held.
                if stream.write_all(&answer).await.is_err() {
                    break;
                }
            }
        }
    });
    Server {
        heard,
        suspends,
        release,
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "the server answers what the driver sends and refuses every other command alike"
)]
fn answer(seq: u32, command: &Command, state: SinkState, lists: Option<&CStr>) -> Vec<u8> {
    match command {
        Command::Auth(params) => reply(
            seq,
            &AuthReply {
                version: params.version,
                ..AuthReply::default()
            },
        ),
        Command::SetClientName(_) => reply(seq, &SetClientNameReply { client_id: 0 }),
        Command::Subscribe(_)
        | Command::SuspendSink(_)
        | Command::GetCardInfoList
        | Command::GetClientInfoList => ack(seq),
        Command::GetServerInfo => reply(seq, &server_info()),
        Command::GetSinkInfoList => reply(seq, &vec![sink(state)]),
        Command::GetSourceInfoList => match lists {
            Some(name) => reply(seq, &vec![source(MICROPHONE_INDEX, name, None)]),
            None => error(seq),
        },
        Command::GetSourceOutputInfoList => reply(seq, &Vec::<SourceOutputInfo>::new()),
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

/// Whether the adapter tells the server down within one call timeout of the paused clock.
async fn told_down(facts: &mut mpsc::Receiver<Input>) -> bool {
    let down = async {
        loop {
            match facts.recv().await {
                Some(Input::Audio(AudioFact::Down)) => return true,
                Some(_) => {}
                None => return false,
            }
        }
    };
    let wait = Duration::from_millis(AUDIO_CALL_TIMEOUT_MS);
    timeout(wait, down).await.unwrap_or(false)
}

/// Waits until the server has `held`, whose answer it holds back.
async fn hears(heard: &mut mpsc::UnboundedReceiver<CommandTag>, held: CommandTag) {
    loop {
        match heard.recv().await {
            Some(tag) if tag == held => return,
            Some(_) => {}
            None => panic!("the server ended before it heard {held:?}"),
        }
    }
}

/// What the adapter tells up to the first fact that is `last`, or in the policy's whole wait for a restore when none is.
async fn told_through(
    facts: &mut mpsc::Receiver<Input>,
    last: impl Fn(AudioFact) -> bool,
) -> Vec<AudioFact> {
    let mut told = Vec::new();
    let through = async {
        while let Some(Input::Audio(fact)) = facts.recv().await {
            told.push(fact);
            if last(fact) {
                return;
            }
        }
    };
    // A wait that runs out leaves what was told, which the test's assertion then shows.
    drop(timeout(Duration::from_millis(RESTORE_WAIT_MS), through).await);
    told
}

fn is_restored(fact: AudioFact) -> bool {
    matches!(fact, AudioFact::Restored(_))
}

/// Every suspend and resume the server has taken and the test has not read yet, in order.
fn taken(suspends: &mut mpsc::UnboundedReceiver<bool>) -> Vec<bool> {
    let mut taken = Vec::new();
    while let Ok(suspend) = suspends.try_recv() {
        taken.push(suspend);
    }
    taken
}

/// The adapter following the device, with the test's ends of it and of the server; all of it ends with the paused runtime.
struct Following {
    requests: mpsc::Sender<AudioRequest>,
    facts: mpsc::Receiver<Input>,
    server: Server,
    _dir: TempDir,
}

/// Inside a paused runtime: the adapter told to follow the device, against a server that holds back its first `held` answer; `named` is the state the microphone's source is read from.
async fn follow(held: CommandTag, named: State) -> Following {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join(SOCKET);
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let server = serve(listener, held, Some(microphone()));
    let (inputs, facts) = mpsc::channel(FACTS);
    let (requests, asked) = queue();
    let state = watch::channel(named).1;
    tokio::spawn(run(inputs, asked, dir.path().to_path_buf(), state));
    let following = Following {
        requests,
        facts,
        server,
        _dir: dir,
    };
    tell_to_follow(&following, DEVICE).await;
    following
}

/// Asks the adapter to follow `device`, as the driver does at every link up.
async fn tell_to_follow(following: &Following, device: &str) {
    let device = device.parse().unwrap();
    let request = AudioRequest::Follow(Some(device));
    following.requests.send(request).await.unwrap();
}

/// The one restore the policy asks for through its first STOP.
fn first_restore() -> RestoreId {
    let (through_the_first, _) = two_restores();
    let [id] = restore_ids(&through_the_first)[..] else {
        panic!("{through_the_first:?}");
    };
    id
}

/// A state whose microphone's source is up, so the adapter reads the record streams on it.
fn source_up() -> State {
    let mut named = State {
        model_number: MODEL_NUMBER.to_owned(),
        ..State::default()
    };
    named.microphone.show(MicCode::Ready, 0, true);
    named
}

/// The name of the source `source_up` shows.
fn microphone() -> CString {
    CString::new(source_up().mic_source()).unwrap()
}

/// The commands the server hears from `held` on, a restore being asked while the answer to `held` is held back; `lists` names the one source the server lists.
fn heard_from(held: CommandTag, lists: &CStr) -> Vec<CommandTag> {
    let (through_the_first, _) = two_restores();
    let [id] = restore_ids(&through_the_first)[..] else {
        panic!("{through_the_first:?}");
    };
    let named = source_up();
    on_paused_clock(async {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join(SOCKET);
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        let mut server = serve(listener, held, Some(lists.to_owned()));
        let (inputs, mut facts) = mpsc::channel(FACTS);
        let (requests, asked) = queue();
        let (_hub, state) = watch::channel(named);
        let adapter = tokio::spawn(run(inputs, asked, dir.path().to_path_buf(), state));
        let device = DEVICE.parse().unwrap();
        requests
            .send(AudioRequest::Follow(Some(device)))
            .await
            .unwrap();
        // The server has the command and holds its answer, so the restore is asked inside that read.
        hears(&mut server.heard, held).await;
        requests.send(AudioRequest::Restore(id)).await.unwrap();
        server.release.send(()).unwrap();
        assert_eq!(restored(&mut facts).await, id);
        drop(requests);
        stop(adapter).await;
        let mut heard = vec![held];
        while let Ok(tag) = server.heard.try_recv() {
            heard.push(tag);
        }
        heard
    })
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
        let mut server = serve(
            UnixListener::bind(&socket).unwrap(),
            CommandTag::SuspendSink,
            None,
        );
        let (inputs, mut facts) = mpsc::channel(FACTS);
        let (requests, asked) = queue();
        let state = watch::channel(State::default()).1;
        let adapter = tokio::spawn(run(inputs, asked, dir.path().to_path_buf(), state));
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

#[test]
fn a_refused_read_of_the_record_streams_holds_back_no_restore() {
    let (through_the_first, _) = two_restores();
    let [id] = restore_ids(&through_the_first)[..] else {
        panic!("{through_the_first:?}");
    };
    // The source is up, so the adapter reads its record streams, which this server refuses.
    let named = source_up();
    let (suspends, answered, lost) = on_paused_clock(async {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join(SOCKET);
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let mut server = serve(
            UnixListener::bind(&socket).unwrap(),
            CommandTag::SuspendSink,
            None,
        );
        let (inputs, mut facts) = mpsc::channel(FACTS);
        let (requests, asked) = queue();
        let (_hub, state) = watch::channel(named);
        let adapter = tokio::spawn(run(inputs, asked, dir.path().to_path_buf(), state));
        let device = DEVICE.parse().unwrap();
        // Queued together, so the restore is owed at the first read of the server.
        requests
            .send(AudioRequest::Follow(Some(device)))
            .await
            .unwrap();
        requests.send(AudioRequest::Restore(id)).await.unwrap();
        server.release.send(()).unwrap();
        let suspends = [server.suspends.recv().await, server.suspends.recv().await];
        let answered = restored(&mut facts).await;
        // Only the refused read loses this server, so the loss told is that read made, after the restore.
        let lost = told_down(&mut facts).await;
        drop(requests);
        stop(adapter).await;
        (suspends, answered, lost)
    });
    assert_eq!(suspends, [Some(true), Some(false)]);
    assert_eq!(answered, id);
    assert!(lost, "the record streams were not read");
}

#[test]
fn a_restore_asked_while_the_sources_are_read_is_done_before_the_record_streams_are_read() {
    let held = CommandTag::GetSourceInfoList;
    let heard = heard_from(held, &microphone());
    let restore = [held, CommandTag::SuspendSink, CommandTag::SuspendSink];
    assert!(heard.starts_with(&restore), "{heard:?}");
}

#[test]
fn a_restore_asked_while_the_record_streams_are_read_is_done_before_the_sink_is_read_again() {
    let held = CommandTag::GetSourceOutputInfoList;
    let heard = heard_from(held, &microphone());
    let restore = [held, CommandTag::SuspendSink, CommandTag::SuspendSink];
    assert!(heard.starts_with(&restore), "{heard:?}");
}

#[test]
fn a_restore_asked_while_the_sources_are_read_is_done_when_the_source_is_not_listed() {
    let held = CommandTag::GetSourceInfoList;
    let heard = heard_from(held, ANOTHER_SOURCE);
    // Nothing follows the restore: the source is not listed, so its record streams are never read.
    assert_eq!(
        heard,
        [held, CommandTag::SuspendSink, CommandTag::SuspendSink]
    );
}

#[test]
fn a_follow_of_the_device_already_followed_keeps_its_restore() {
    let id = first_restore();
    let (told, suspends) = on_paused_clock(async {
        let held = CommandTag::GetSourceInfoList;
        let mut following = follow(held, source_up()).await;
        hears(&mut following.server.heard, held).await;
        // Both wait in the queue behind the held read, so the restore is asked and not yet done when the follow is taken.
        let restore = AudioRequest::Restore(id);
        following.requests.send(restore).await.unwrap();
        tell_to_follow(&following, DEVICE).await;
        following.server.release.send(()).unwrap();
        let told = told_through(&mut following.facts, is_restored).await;
        (told, taken(&mut following.server.suspends))
    });
    // The driver forgets the server and the sink at every link up, so both are told again ahead of the answer.
    let [
        AudioFact::Up,
        AudioFact::Sink(Some(first)),
        AudioFact::Up,
        AudioFact::Sink(Some(second)),
        AudioFact::Restored(answered),
    ] = told[..]
    else {
        panic!("{told:?}");
    };
    assert_eq!((second, answered), (first, id));
    assert_eq!(suspends, [true, false]);
}

#[test]
fn a_follow_of_the_device_already_followed_undoes_the_suspend_left_on_its_sink() {
    let id = first_restore();
    let (told, suspends) = on_paused_clock(async {
        let mut following = follow(CommandTag::SuspendSink, State::default()).await;
        let restore = AudioRequest::Restore(id);
        following.requests.send(restore).await.unwrap();
        // The suspend's answer is held past the call timeout, so the server is lost with the sink left suspended.
        let lost = told_through(&mut following.facts, |fact| fact == AudioFact::Down).await;
        assert_eq!(lost.last(), Some(&AudioFact::Down), "{lost:?}");
        assert_eq!(taken(&mut following.server.suspends), [true]);
        tell_to_follow(&following, DEVICE).await;
        following.server.release.send(()).unwrap();
        let told = told_through(&mut following.facts, is_restored).await;
        (told, taken(&mut following.server.suspends))
    });
    assert_eq!(told.last(), Some(&AudioFact::Restored(id)), "{told:?}");
    // The suspend is not sent again, only undone.
    assert_eq!(suspends, [false]);
}

#[test]
fn a_follow_of_another_device_ends_the_follow_of_the_one_before() {
    let told = on_paused_clock(async {
        let held = CommandTag::GetSourceInfoList;
        let mut following = follow(held, source_up()).await;
        // The first device's sink is told by now, so the next facts are the other device's.
        hears(&mut following.server.heard, held).await;
        tell_to_follow(&following, ANOTHER_DEVICE).await;
        following.server.release.send(()).unwrap();
        told_through(&mut following.facts, |fact| fact == AudioFact::Sink(None)).await
    });
    // The other device has no sink here, so a sink told for it would be the first device's, still followed.
    let [
        AudioFact::Up,
        AudioFact::Sink(Some(_)),
        AudioFact::Up,
        AudioFact::Sink(None),
    ] = told[..]
    else {
        panic!("{told:?}");
    };
}
