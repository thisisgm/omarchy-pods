//! Runs the link machine: events, SDUs, commands and the clock in; state and actions out.

mod reader;

use std::collections::{HashMap, VecDeque};
use std::future::{self, poll_fn};
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::Poll;
use std::thread;
use std::time::Duration;

use pods_core::command::{Command, Outcome};
use pods_core::link::{
    Action, Call, DeviceAction, End, Event, Machine, Phase, STATE_POPULATED_LINE, Signal, Ticket,
    phase_change_line,
};
use pods_core::policy::audio::{AudioFact, AudioRequest, server_after};
use pods_core::policy::notices::Notice;
use pods_core::policy::{
    DictationFact, DictationRequest, Effect, Era, Fact, MicFact, MonitorFact, PlayerOwner, Policy,
    Sighting, SleepEdge, UplinkRequest,
};
use pods_core::prefs::Prefs;
use pods_core::state::{MicCounts, MicSpan, State};
use pods_mic::Counters;
use pods_proto::addr::BdAddr;
use pods_proto::doap::{doap_mode_off, doap_mode_on};
use pods_proto::keys::ProximityKeys;
use pods_proto::model::Model;
use pods_proto::uplink::UplinkPacket;
use pods_sys::l2cap::{Channel, ChannelError};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{self, JoinHandle};
use tokio::time::{Instant, timeout, timeout_at};
use zeroize::Zeroizing;

use super::hub::Hub;
use super::{MAX_CONNECTIONS, audio, dictation, keys, log, media, mic, notify, sleep};
pub(super) use reader::Open;

/// From aacp.rs in librepods-org/librepods pull request 655; a connect waits this long at most.
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// One command per client, shared with device events, sleep edges and connect answers; when full those wait and a command is refused.
pub(super) const INPUT_QUEUE: usize = MAX_CONNECTIONS;

/// One sweep reads every device, so a few waiting cover any burst of events.
pub(super) const REQUEST_QUEUE: usize = 8;

/// The slowest measured START answer, STOP answer and restore hold together are about 1.2 s.
const MIC_EXIT_MS: u64 = 2000;

/// Covers the uplink's exit, so the driver is not aborted inside it, plus what the other stop steps had; a bound, not a wait.
pub(super) const STOP_WAIT_MS: u64 = MIC_EXIT_MS + 500;

const CONNECT_THREAD: &str = "link-connect";

const EXIT_BEFORE_RESTORE_LINE: &str = "mic: exit before restore";
const EXIT_UPLINK_RUNNING_LINE: &str = "mic: exit with the uplink running, the sink is playing";
const DECODER_RESET_LINE: &str = "mic: decoder reset";
const RECORDER_LINKED_LINE: &str = "mic: recorder linked";

#[derive(Debug, Clone, Copy)]
pub(super) enum DeviceEvent {
    Up(BdAddr),
    Down(BdAddr),
    Gone(BdAddr),
    Lost,
    CallFailed(Call),
}

#[derive(Debug)]
pub(super) enum Input {
    Device(DeviceEvent),
    /// From the connect thread, which ends after this or after its `Ended`; the device is the one it dialed.
    Opened(BdAddr, Channel),
    Ended(End),
    /// Made by the driver itself when a write fails.
    SendFailed,
    Command {
        command: Command,
        reply: oneshot::Sender<Outcome>,
    },
    Sleep(SleepEdge),
    Shutdown(oneshot::Sender<()>),
    Audio(AudioFact),
    /// The players a pause of that era paused.
    Media(Era, Vec<PlayerOwner>),
    /// A pause of that era ended after the count of link losses moved.
    PauseCut(Era),
    Advert(Sighting),
    Monitor(MonitorFact),
    Mic(MicFact),
    Dictation(dictation::ArmId, DictationFact),
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Request {
    Sweep,
    Call {
        device: BdAddr,
        call: Call,
        when_worn: bool,
    },
}

/// Where the policy's notices, choices, audio and media requests and keys go, one queue per adapter.
#[derive(Debug)]
pub(super) struct Outlets {
    pub(super) notices: mpsc::Sender<Notice>,
    pub(super) prefs: watch::Sender<Prefs>,
    pub(super) audio: mpsc::Sender<AudioRequest>,
    pub(super) keys: mpsc::Sender<ProximityKeys>,
    pub(super) media: media::Outlet,
    /// Whether the advert monitor is wanted; a watch, so the latest is never dropped, and each `true` registers it anew.
    pub(super) monitor: watch::Sender<bool>,
    /// The latest source request; a watch, so turns of the setting while an open hangs never drop the last.
    pub(super) mic: watch::Sender<mic::Request>,
    /// The adapter of the link's attribute channel, started and ended with the control channel.
    pub(super) dictation: dictation::Outlet,
    pub(super) sleep_bus: sleep::Bus,
    /// Where the pods' audio goes, shared with the source's node.
    pub(super) tap: Arc<mic::Tap>,
}

pub(super) type Connector = Arc<dyn Fn(BdAddr) -> Result<Channel, ChannelError> + Send + Sync>;

/// The one place the queue's bound is set.
pub(super) fn queue() -> (mpsc::Sender<Input>, mpsc::Receiver<Input>) {
    mpsc::channel(INPUT_QUEUE)
}

/// A thread still inside its connect is left to the end of the process.
pub(super) async fn stop(link: &mpsc::Sender<Input>, driver: JoinHandle<()>) {
    let (done, stopped) = oneshot::channel();
    let asked = async {
        if link.send(Input::Shutdown(done)).await.is_ok() {
            // An error is a driver that ended without answering, which is stopped all the same.
            stopped.await.ok();
        }
    };
    if timeout(Duration::from_millis(STOP_WAIT_MS), asked)
        .await
        .is_err()
    {
        log(&format!(
            "link: the driver did not stop within {STOP_WAIT_MS} ms; aborting it"
        ));
    }
    driver.abort();
    // Awaited so the driver's channel and replies drop first; the result is only the cancellation.
    drop(driver.await);
}

pub(super) async fn drive(
    hub: Hub,
    mut inputs: mpsc::Receiver<Input>,
    feedback: mpsc::Sender<Input>,
    requests: mpsc::Sender<Request>,
    connector: Connector,
    outlets: Outlets,
    keys: Option<ProximityKeys>,
) {
    let mut driver = Driver::new(hub, feedback, requests, connector, outlets, keys);
    for effect in driver.policy.start() {
        driver.route(effect);
    }
    loop {
        let turn = match driver.fed_back.pop_front() {
            Some(input) => Turn::Input(input),
            None => driver.wait(&mut inputs).await,
        };
        match turn {
            Turn::Input(input) => driver.take(input),
            Turn::Read(Some(sdu)) => driver.packet(&sdu),
            Turn::Read(None) => driver.take(Input::Ended(End::Closed)),
            Turn::Tick => driver.tick(),
        }
        if let Some(done) = driver.exited() {
            driver.step(Event::Shutdown);
            // A stop that stopped waiting has nobody left to tell.
            done.send(()).ok();
            return;
        }
        // One change, then every subscriber gets its turn.
        task::yield_now().await;
    }
}

enum Turn {
    Input(Input),
    /// One SDU from the open channel, or `None` at its end.
    Read(Option<Zeroizing<Box<[u8]>>>),
    Tick,
}

struct Driver {
    hub: Hub,
    machine: Machine,
    policy: Policy,
    outlets: Outlets,
    started: Instant,
    feedback: mpsc::Sender<Input>,
    requests: mpsc::Sender<Request>,
    connector: Connector,
    /// The open channel, from its `Opened` until its close or its end.
    channel: Option<Open>,
    /// Whether the last wait asked the channel before the queue; the next wait asks the other first.
    channel_first: bool,
    /// At most one outlives its turn, a written setting the machine answers in the next step.
    replies: HashMap<Ticket, oneshot::Sender<Outcome>>,
    next_ticket: u64,
    /// At most one per action of a step, taken before the queue.
    fed_back: VecDeque<Input>,
    /// `STATE_POPULATED_LINE` was logged since the link last came up.
    announced: bool,
    exit: Option<Exit>,
    dictation_inhibitor: Option<sleep::Inhibitor>,
    /// The decoder resets counted at the last tick.
    resets: u32,
    /// The lifetime counters at the open edge of the current recorder span.
    mic_span_start: Option<MicCounts>,
}

/// A shutdown held until the uplink's exit is done or `by` passes, on the clock of `started`.
struct Exit {
    done: oneshot::Sender<()>,
    by: Duration,
    exited: bool,
}

impl Driver {
    fn new(
        hub: Hub,
        feedback: mpsc::Sender<Input>,
        requests: mpsc::Sender<Request>,
        connector: Connector,
        outlets: Outlets,
        keys: Option<ProximityKeys>,
    ) -> Self {
        Self {
            hub,
            machine: Machine::new(),
            policy: Policy::new(keys),
            outlets,
            started: Instant::now(),
            feedback,
            requests,
            connector,
            channel: None,
            channel_first: false,
            replies: HashMap::new(),
            next_ticket: 0,
            fed_back: VecDeque::new(),
            announced: false,
            exit: None,
            dictation_inhibitor: None,
            resets: 0,
            mic_span_start: None,
        }
    }

    async fn wait(&mut self, inputs: &mut mpsc::Receiver<Input>) -> Turn {
        let deadline = self
            .machine
            .deadline()
            .into_iter()
            .chain(self.policy.deadline())
            .chain(self.exit.as_ref().map(|exit| exit.by))
            .min()
            .and_then(|at| self.started.checked_add(at));
        let next = next_of(inputs, self.channel.as_mut(), &mut self.channel_first);
        let mut next = pin!(async {
            match deadline {
                // Served before the queue and the channel, which stay ready for as long as their peers keep talking.
                Some(at) if at <= Instant::now() => Turn::Tick,
                Some(at) => timeout_at(at, next).await.unwrap_or(Turn::Tick),
                None => next.await,
            }
        });
        poll_fn(|context| {
            let turn = next.as_mut().poll(context);
            if let Some(inhibitor) = &mut self.dictation_inhibitor
                && inhibitor.poll_resolved(context).is_ready()
            {
                self.policy.set_dictation_inhibit_pending(false);
                // A queued sleep or recorder edge can cancel before the new START deadline is checked.
                if turn.is_pending() {
                    return Poll::Ready(Turn::Tick);
                }
            }
            turn
        })
        .await
    }

    fn take(&mut self, input: Input) {
        match input {
            Input::Device(device) => self.step(device.event()),
            Input::Opened(device, channel) => match Open::watch(channel) {
                Ok(open) => {
                    self.channel = Some(open);
                    let (inputs, tap) = (self.feedback.clone(), Arc::clone(&self.outlets.tap));
                    self.outlets.dictation.open(device, inputs, tap);
                    self.step(Event::ChannelOpened);
                }
                Err(error) => {
                    log(&format!("link: cannot watch the channel: {error}"));
                    self.step(Event::ChannelEnded(End::ConnectFailed));
                }
            },
            Input::Ended(end) => {
                self.channel = None;
                self.outlets.dictation.close();
                self.step(Event::ChannelEnded(end));
            }
            Input::SendFailed => self.step(Event::SendFailed),
            Input::Command { command, reply } => {
                let ticket = Ticket(self.next_ticket);
                self.next_ticket = self.next_ticket.wrapping_add(1);
                self.replies.insert(ticket, reply);
                self.step(Event::Command { ticket, command });
            }
            Input::Sleep(edge) => self.observe(Fact::Sleep(edge)),
            Input::Shutdown(done) => {
                let bound = Duration::from_millis(MIC_EXIT_MS);
                let by = self.started.elapsed().saturating_add(bound);
                self.exit = Some(Exit {
                    done,
                    by,
                    exited: false,
                });
                self.observe(Fact::Exit);
            }
            Input::Audio(fact) => self.observe(Fact::Audio(fact)),
            Input::Media(era, paused) => self.observe(Fact::Paused(era, paused)),
            Input::PauseCut(era) => self.observe(Fact::PauseCut(era)),
            Input::Advert(sighting) => self.observe(Fact::Advert(sighting)),
            Input::Monitor(fact) => self.observe(Fact::Monitor(fact)),
            Input::Mic(fact) => self.observe(Fact::Mic(fact)),
            Input::Dictation(arm, fact) => {
                if self.outlets.dictation.current(arm) {
                    self.observe(Fact::Dictation(fact));
                }
            }
        }
    }

    /// An audio SDU goes to the tap and nowhere else; every other packet steps the link machine.
    fn packet(&mut self, sdu: &[u8]) {
        let Ok(Some(UplinkPacket::Audio(audio))) = UplinkPacket::parse(sdu) else {
            self.step(Event::Packet(sdu));
            return;
        };
        if !self.policy.mic_accepts_uplink() {
            self.outlets.tap.count_uplink();
        } else if self.outlets.tap.audio(&audio) {
            self.observe(Fact::Mic(MicFact::FirstAudio));
        }
    }

    /// The clock's step, then a decoder reset since the last tick and each stream's count for its stall check.
    fn tick(&mut self) {
        self.step(Event::Tick);
        let resets = self
            .outlets
            .tap
            .counters()
            .decoder_resets
            .load(Ordering::Relaxed);
        if resets != self.resets {
            log(DECODER_RESET_LINE);
            self.resets = resets;
        }
        let packets = self.outlets.tap.packets();
        self.observe(Fact::Mic(MicFact::Heard { packets }));
        self.observe(heard(&self.outlets.tap));
    }

    /// The policy's recorder edges bound the span, whose status is refreshed at phase changes and existing ticks.
    fn fold(&mut self, last: &State, next: &mut State, tick: bool) {
        let counters = self.outlets.tap.counters();
        let changed = next.microphone.code() != last.microphone.code();
        let recording = self.policy.mic_recording();
        let left = !recording && self.mic_span_start.is_some();
        if left || changed {
            next.microphone.fold(mic_counts(counters));
        }
        match (self.mic_span_start, recording) {
            (None, true) => {
                counters.reset_span();
                self.mic_span_start = Some(mic_counts(counters));
                next.microphone.fold_span(MicSpan::EMPTY);
                log(RECORDER_LINKED_LINE);
            }
            (Some(start), false) => {
                self.mic_span_start = None;
                let span = mic_span(counters);
                next.microphone.fold_span(span);
                log_recorder_left(start, next.microphone.counts(), span);
            }
            (Some(_), true) if tick || changed => next.microphone.fold_span(mic_span(counters)),
            _ => {}
        }
    }

    /// The held shutdown's answer, once the uplink's exit is done or out of time.
    fn exited(&mut self) -> Option<oneshot::Sender<()>> {
        let exit = self.exit.take()?;
        if exit.exited {
            return Some(exit.done);
        }
        if self.started.elapsed() >= exit.by {
            log(if self.policy.mic_stop_waits() {
                EXIT_UPLINK_RUNNING_LINE
            } else {
                EXIT_BEFORE_RESTORE_LINE
            });
            return Some(exit.done);
        }
        self.exit = Some(exit);
        None
    }

    /// The link machine, then the server reading and the policy on its signals, a changed state and the clock, then one publish, then the effects.
    fn step(&mut self, event: Event<'_>) {
        let before = self.machine.phase();
        let last = self.hub.state();
        // BlueZ gives no reason, so an unrequested drop holds reconnects if the buds were worn at link loss.
        let external_disconnect = matches!(event, Event::DeviceDown(device)
            if self.machine.unrequested_down_while_worn(device, &last));
        let mut next = last.clone();
        let now = self.started.elapsed();
        let actions = self.machine.step(&mut next, event, now);
        if external_disconnect
            || actions.iter().any(|action| {
                matches!(action, Action::Call { call, .. } if call.action == DeviceAction::Disconnect)
            })
        {
            self.policy.manual_disconnect();
        }
        self.log_progress(before, &next);
        let mut facts = Vec::new();
        for action in &actions {
            if let Action::Signal(signal) = action {
                if let Signal::Down | Signal::Lost { .. } = signal {
                    self.outlets.media.link_lost();
                }
                facts.push(Fact::Link(signal.clone()));
            }
        }
        if next != last {
            facts.push(Fact::Changed);
        }
        if event == Event::Tick {
            facts.push(Fact::Tick);
        }
        for fact in &facts {
            next.audio_server = server_after(next.audio_server, fact);
            self.policy.apply(&mut next, fact);
        }
        let effects: Vec<Effect> = facts
            .into_iter()
            .flat_map(|fact| self.policy.step(&next, fact, now))
            .collect();
        self.policy.show_mic(&mut next);
        self.fold(&last, &mut next, event == Event::Tick);
        self.publish(next);
        for action in actions {
            self.execute(action);
        }
        self.route_effects(effects);
    }

    /// A fact from outside the link machine: the server reading and the policy fold it in, then the policy steps on that state.
    fn observe(&mut self, fact: Fact) {
        let last = self.hub.state();
        let mut next = last.clone();
        let now = self.started.elapsed();
        next.audio_server = server_after(next.audio_server, &fact);
        let connect = if let Fact::Advert(sighting) = &fact {
            self.policy.advert(&mut next, sighting, now)
        } else {
            self.policy.apply(&mut next, &fact);
            false
        };
        let worn_call = if connect && self.exit.is_none() {
            self.machine.connect_when_worn(&mut next)
        } else {
            None
        };
        let mut effects = self.policy.step(&next, fact, now);
        if next != last {
            effects.extend(self.policy.step(&next, Fact::Changed, now));
        }
        self.policy.show_mic(&mut next);
        self.fold(&last, &mut next, false);
        self.publish(next);
        if let Some((device, call)) = worn_call {
            self.policy.connect_attempted(now);
            self.call(device, call, true);
        }
        self.route_effects(effects);
    }

    /// Publishes preferences and state only when each changes.
    fn publish(&self, next: State) {
        let prefs = Prefs::of(&next);
        self.outlets.prefs.send_if_modified(|kept| {
            let changed = *kept != prefs;
            if changed {
                *kept = prefs;
            }
            changed
        });
        self.hub.publish(next);
    }

    fn route_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            self.route(effect);
        }
        if !self.policy.dictation_session_open() {
            self.dictation_inhibitor = None;
        }
    }

    /// After the step's own publish and actions, so a wake is a step of its own.
    fn route(&mut self, effect: Effect) {
        match effect {
            Effect::Notify(notice) => notify::offer(&self.outlets.notices, notice),
            Effect::Wake => self.step(Event::Resumed),
            Effect::Audio(request) => audio::offer(&self.outlets.audio, request),
            Effect::StoreKeys(keys) => keys::offer(&self.outlets.keys, keys),
            Effect::Media(request) => self.outlets.media.offer(request),
            Effect::Monitor(wanted) => drop(self.outlets.monitor.send_replace(wanted)),
            Effect::Uplink(request) => {
                if request == UplinkRequest::Start {
                    self.outlets.tap.arm();
                }
                self.send(request.sdu());
            }
            Effect::Mic(request) => {
                mic::offer(&self.outlets.mic, request, self.machine.connected_device());
            }
            Effect::Dictation(DictationRequest::Mode) => {
                self.send(&doap_mode_on());
                if self.dictation_inhibitor.is_none() {
                    self.dictation_inhibitor = sleep::inhibit(&self.outlets.sleep_bus);
                    self.policy
                        .set_dictation_inhibit_pending(self.dictation_inhibitor.is_some());
                }
            }
            Effect::Dictation(DictationRequest::ModeOff) => {
                self.send(&doap_mode_off());
                self.outlets.tap.end_dictation();
            }
            Effect::Dictation(request) => self.outlets.dictation.offer(request),
            Effect::Log(line) => log(&line),
            Effect::Exited => {
                if let Some(exit) = &mut self.exit {
                    exit.exited = true;
                }
            }
        }
    }

    fn execute(&mut self, action: Action) {
        match action {
            Action::OpenChannel(device) => self.open(device),
            Action::CloseChannel => self.close(),
            Action::Send(sdu) => self.send(&sdu),
            Action::Sweep => {
                // Dropped when the queue is full: nothing waits on a sweep's answer.
                self.requests.try_send(Request::Sweep).ok();
            }
            Action::Call { device, call } => self.call(device, call, false),
            Action::Resolve { ticket, outcome } => {
                if let Some(reply) = self.replies.remove(&ticket) {
                    // A client that left no longer waits for its answer.
                    reply.send(outcome).ok();
                }
            }
            // Already handed to the policy, before the publish.
            Action::Signal(_) => {}
        }
    }

    fn call(&mut self, device: BdAddr, call: Call, when_worn: bool) {
        if self
            .requests
            .try_send(Request::Call {
                device,
                call,
                when_worn,
            })
            .is_err()
        {
            let failed = DeviceEvent::CallFailed(call);
            self.fed_back.push_back(Input::Device(failed));
        }
    }

    fn open(&mut self, device: BdAddr) {
        let connector = Arc::clone(&self.connector);
        let feedback = self.feedback.clone();
        // Never joined: a thread inside connect cannot be woken; the end of the process ends it.
        let spawned = thread::Builder::new()
            .name(CONNECT_THREAD.to_owned())
            .spawn(move || {
                let answer = match connector(device) {
                    Ok(channel) => Input::Opened(device, channel),
                    Err(error) => {
                        log(&format!("link: {error}"));
                        Input::Ended(End::ConnectFailed)
                    }
                };
                // A driver that is gone has stopped and needs no answer.
                feedback.blocking_send(answer).ok();
            });
        if let Err(error) = spawned {
            log(&format!("link: cannot start the connect thread: {error}"));
            self.fed_back.push_back(Input::Ended(End::ConnectFailed));
        }
    }

    fn close(&mut self) {
        // Dropping the channel closes it, and the machine waits for the end of each channel it closes.
        if self.channel.take().is_some() {
            self.fed_back.push_back(Input::Ended(End::Closed));
        }
    }

    fn send(&mut self, sdu: &[u8]) {
        let Some(open) = &self.channel else {
            return;
        };
        if let Err(error) = open.channel().try_send(sdu) {
            log(&format!("link: {error}"));
            // First, so the machine learns of it before any input that came after the write.
            self.fed_back.push_front(Input::SendFailed);
        }
    }

    fn log_progress(&mut self, before: Phase, state: &State) {
        let after = self.machine.phase();
        // The stop already has its own line, the signal's.
        if before.name() != after.name() && after != Phase::Stopped {
            log(&phase_change_line(before, after));
            self.announced = false;
        }
        if matches!(after, Phase::Up { .. }) && !self.announced && populated(state) {
            log(STATE_POPULATED_LINE);
            self.announced = true;
        }
    }
}

/// The queue's next input or the channel's next SDU; `channel_first` changes every call, so neither keeps the other from its turn.
async fn next_of(
    inputs: &mut mpsc::Receiver<Input>,
    channel: Option<&mut Open>,
    channel_first: &mut bool,
) -> Turn {
    *channel_first = !*channel_first;
    let channel_first = *channel_first;
    let mut read = pin!(async {
        match channel {
            Some(open) => open.read().await,
            None => future::pending().await,
        }
    });
    poll_fn(|context| {
        if channel_first && let Poll::Ready(sdu) = read.as_mut().poll(context) {
            return Poll::Ready(Turn::Read(sdu));
        }
        // A closed queue never comes: the driver holds `feedback`, a sender of this same queue.
        if let Poll::Ready(Some(input)) = inputs.poll_recv(context) {
            return Poll::Ready(Turn::Input(input));
        }
        if channel_first {
            return Poll::Pending;
        }
        read.as_mut().poll(context).map(Turn::Read)
    })
    .await
}

impl DeviceEvent {
    fn event(self) -> Event<'static> {
        match self {
            Self::Up(device) => Event::DeviceUp(device),
            Self::Down(device) => Event::DeviceDown(device),
            Self::Gone(device) => Event::DeviceGone(device),
            Self::Lost => Event::BluezLost,
            Self::CallFailed(call) => Event::CallFailed(call),
        }
    }
}

fn heard(tap: &mic::Tap) -> Fact {
    Fact::Dictation(DictationFact::Heard {
        packets: tap.dictation_packets(),
    })
}

fn mic_counts(counters: &Counters) -> MicCounts {
    MicCounts {
        underruns: counters.underruns.load(Ordering::Relaxed),
        concealed: counters.concealed.load(Ordering::Relaxed),
        decode_errors: counters.decode_errors.load(Ordering::Relaxed),
        limited: counters.limited.load(Ordering::Relaxed),
        nonfinite: counters.nonfinite.load(Ordering::Relaxed),
    }
}

fn mic_span(counters: &Counters) -> MicSpan {
    MicSpan::new(
        counters.span_sdus.load(Ordering::Relaxed),
        counters.span_restarts.load(Ordering::Relaxed),
        counters.span_gone_frames.load(Ordering::Relaxed),
        counters.decoded_peak(),
    )
}

fn log_recorder_left(start: MicCounts, end: MicCounts, span: MicSpan) {
    let underruns = end.underruns.wrapping_sub(start.underruns);
    let concealed = end.concealed.wrapping_sub(start.concealed);
    let limited = end.limited.wrapping_sub(start.limited);
    let peak = span
        .peak_dbfs()
        .map_or_else(|| "silence".to_owned(), |dbfs| format!("{dbfs:.1} dBFS"));
    log(&format!(
        "mic: recorder left (this span: underruns {underruns}, concealed {concealed}, limited {limited}, accepted SDUs {}, run restarts {}, Gone frames {}, decoded peak {peak})",
        span.sdus, span.restarts, span.gone_frames
    ));
}

/// A name, a model and one battery reading: what the bar shows first.
fn populated(state: &State) -> bool {
    let batteries = state.batteries;
    let any_battery = [
        batteries.left,
        batteries.right,
        batteries.case,
        batteries.headset,
    ]
    .iter()
    .any(Option::is_some);
    !state.device_name.is_empty() && state.model != Model::Unknown && any_battery
}

#[cfg(test)]
pub(super) mod tests;
