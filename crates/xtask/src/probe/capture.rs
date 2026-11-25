//! The capture loop: one thread reads the open channels, stamps and logs every packet, and follows a script.

mod answer;
mod att;
mod guard;
mod record;
mod wires;

use std::cell::RefCell;
use std::convert::Infallible;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pods_proto::addr::BdAddr;
use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::dictation;
use pods_proto::ear::EarReport;
use pods_proto::handshake::{Handshake, Step};
use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
use pods_proto::uplink::{self, MicSide};
use pods_sys::l2cap::{self, Channel, ChannelError};

use super::dictate::{self, HANDLE_WALK_TIMEOUT_MS, HandleCheck};
use super::script::{Action, Opening, Script, StepPacket};
use answer::NoLine;
use att::Asked;
use guard::{Dictation, Dropped, Session, SideChange, Tail, Uplink};
use record::Recorder;
pub(crate) use record::{
    ATT_LOG_NAME, Bearer, HANDLES_NAME, IoFailure, LOG_NAME, MARKS_NAME, Mark, Outputs, mark_line,
    write_mark,
};
pub(crate) use wires::OpenAtt;
use wires::{RECV_TIMEOUT_MS, Wake, Wires, receive};

/// How long one handshake request may go unanswered: daemon/main.cpp asks again after 2 s, the probe fails naming the step.
pub(crate) const HANDSHAKE_STEP_TIMEOUT_MS: u64 = 2_000;
/// How long START waits for an ear detection packet: kEarReportWaitMs in pull request 85 of this repository.
const EAR_REPORT_WAIT_MS: u64 = 3_000;
/// The receive buffer of aacp.rs in librepods-org/librepods pull request 655.
const RECV_BUFFER_LEN: usize = 4096;
/// How long the channel stays open after STOP: pull request 85 of this repository flushes for up to 1 s before it closes.
pub(crate) const STOP_TAIL_MS: u64 = 1_000;
/// How long a side change waits for the pods to report it; chosen to match the ear wait.
const SIDE_REPORT_WAIT_MS: u64 = 3_000;
const NS_PER_MS: u64 = 1_000_000;

/// Each wait in nanoseconds; a test passes shorter waits.
#[derive(Debug, Clone, Copy)]
struct Timing {
    handshake_step: u64,
    ear_report: u64,
    side_report: u64,
    /// How long each request on the attribute channel may go unanswered, in the milliseconds its constant is written in.
    handle_walk_ms: u32,
    stop_tail: u64,
}

impl Timing {
    const REAL: Self = Self {
        handshake_step: HANDSHAKE_STEP_TIMEOUT_MS * NS_PER_MS,
        ear_report: EAR_REPORT_WAIT_MS * NS_PER_MS,
        side_report: SIDE_REPORT_WAIT_MS * NS_PER_MS,
        handle_walk_ms: HANDLE_WALK_TIMEOUT_MS,
        stop_tail: STOP_TAIL_MS * NS_PER_MS,
    };
}

/// The open control channel, and an attribute channel opener taken at the opening or after the handshake.
#[derive(Clone, Copy)]
pub(crate) struct Links<'a> {
    pub(crate) accessory: Option<&'a Channel>,
    pub(crate) att: Option<OpenAtt<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EndReason {
    ScriptDone,
    Interrupted,
    PeerClosed,
    AttConnect(ChannelError),
    AttClosed,
    /// The pods' own listing did not show the four handles of the stream, so the start write was not sent.
    HandleCheck(HandleCheck),
    /// The pods listed the four handles and the walk was cut before they ended their table.
    WalkUnfinished,
    HandshakeTimeout(Step),
    NoInitialSide,
    /// A cue of a step file got no line on standard input.
    Cue(NoLine),
    Io(IoFailure),
}

impl fmt::Display for EndReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScriptDone => f.write_str("the script ran to its end"),
            Self::Interrupted => f.write_str("interrupted by a signal"),
            Self::PeerClosed => f.write_str("the pods closed the channel"),
            Self::AttConnect(error) => write!(
                f,
                "the attribute channel on PSM {ATT_PSM:#06x} did not open: {error}"
            ),
            Self::AttClosed => f.write_str("the pods closed the attribute channel"),
            Self::HandleCheck(HandleCheck::NoListing) => {
                f.write_str("the pods listed no attribute, so the start write was not sent")
            }
            Self::HandleCheck(check @ (HandleCheck::Missing(_) | HandleCheck::Present)) => write!(
                f,
                "the pods' listing of the handles the stream uses read {check}, so the start write was not sent"
            ),
            Self::WalkUnfinished => f.write_str(
                "the walk of the pods' attribute table was cut before its end, so the start write was not sent",
            ),
            Self::HandshakeTimeout(step) => {
                write!(f, "the handshake timed out waiting for {step}")
            }
            Self::NoInitialSide => f.write_str(
                "the pods did not report their microphone side, so no setting was changed",
            ),
            Self::Cue(no_line) => write!(f, "{no_line}"),
            Self::Io(failure) => write!(f, "{failure}"),
        }
    }
}

impl From<IoFailure> for EndReason {
    fn from(failure: IoFailure) -> Self {
        Self::Io(failure)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    /// Packets from the pods that went into the log.
    pub(crate) sdus_in: u64,
    /// The largest incoming control SDU, including any truncated read.
    pub(crate) largest_sdu: usize,
    /// Those of them that carry coded audio.
    pub(crate) audio_sdus: u64,
    /// START packets that left.
    pub(crate) starts: u32,
    /// STOP packets that left.
    pub(crate) stops: u32,
    /// A START left and no STOP has left since.
    pub(crate) uplink_running: bool,
    /// Packets marked and not logged: longer than the receive buffer, or refused by the log format.
    pub(crate) truncated: u32,
    /// Packets of the attribute channel from the pods that went into its log.
    pub(crate) att_pdus_in: u64,
    /// Those of them that are audio notifications of the dictation stream.
    pub(crate) audio_notifications: u64,
    /// Dictation start writes that left.
    pub(crate) dictation_starts: u32,
    /// Dictation stop writes that left.
    pub(crate) dictation_stops: u32,
    pub(crate) att_requests_refused: u32,
    pub(crate) end: EndReason,
}

/// `Err` is why the run ends, which for a script that ran to its end is no failure.
type Flow = Result<(), EndReason>;

/// Where the run is; each phase holds what only it needs.
enum Phase {
    /// A request went out at `asked_at_ns` and its acknowledgement has not arrived.
    Handshaking {
        handshake: Handshake,
        asked_at_ns: u64,
    },
    /// The notification request went out at `asked_at_ns`; the gate waits for what the pods report.
    Settling { asked_at_ns: u64 },
    /// The gate opened at `gate_ns`; the events before `next` have fired.
    Scripted { gate_ns: u64, next: usize },
    /// A cue of a step file is shown, `offset_ns` after the gate; the events from `next` on wait for a line on standard input, which then stands where the cue stood.
    Held { next: usize, offset_ns: u64 },
}

/// One run: the loop's state and its guards, which drop with it on every way out.
struct Engine<'a, 'o> {
    wires: Wires<'a>,
    /// How the run opens the attribute channel; `None` for a run that was given no way to.
    open_att: Option<OpenAtt<'a>>,
    script: &'a Script,
    recorder: &'a mut Recorder<'o>,
    interrupted: &'a AtomicBool,
    timing: &'a Timing,
    dropped: &'a Dropped,
    phase: Phase,
    /// An ear detection packet arrived before the gate.
    ear_seen: bool,
    /// The microphone side the pods reported before the gate.
    initial_side: Option<ControlCommand>,
    /// The channel the last packet came from, which the next wait asks last.
    last_read: Bearer,
    asked: Asked,
    /// The walk found the four handles, or the owner waived a listing the pods did not give.
    checked: bool,
    /// When the last dictation start write left, which the delay of the pods' reply counts from.
    start_sent_ns: Option<u64>,
    // Fields drop in this order: each stream is stopped, then the session is marked idle, then the side is put back.
    uplink: Option<Uplink<'a>>,
    dictation: Option<Dictation<'a>>,
    session: Option<Session<'a>>,
    side: Option<SideChange<'a>>,
}

impl Engine<'_, '_> {
    fn drive(&mut self) -> EndReason {
        let Err(end) = self.steps();
        end
    }

    fn steps(&mut self) -> Result<Infallible, EndReason> {
        if let Some(channel) = self.wires.accessory {
            let timed = channel.set_recv_timeout(Duration::from_millis(RECV_TIMEOUT_MS));
            timed.map_err(|error| IoFailure::Channel(Bearer::Accessory, error))?;
            let at_ns = self.wires.now();
            self.recorder.mark(at_ns, Mark::Connect, &"channel open")?;
        }
        // A signal that arrived during the connect ends the run before anything is sent.
        self.check_interrupted()?;
        self.open()?;
        let mut buf = [0_u8; RECV_BUFFER_LEN];
        loop {
            self.turn(&mut buf)?;
        }
    }

    /// One wait and what follows from it: the packet, the signal flag, then the clock.
    fn turn(&mut self, buf: &mut [u8]) -> Flow {
        let (at_ns, wake) = receive(&self.wires, self.last_read, buf);
        self.take(at_ns, wake)?;
        self.check_interrupted()?;
        self.advance(at_ns)
    }

    fn check_interrupted(&self) -> Flow {
        if self.interrupted.load(Ordering::SeqCst) {
            Err(EndReason::Interrupted)
        } else {
            Ok(())
        }
    }

    fn open(&mut self) -> Flow {
        match self.script.opening {
            Opening::Knock => self.open_gate("connect"),
            Opening::AttKnock => {
                self.connect_att()?;
                self.open_gate("connect")
            }
            Opening::AttMap => {
                self.connect_att()?;
                let asked_at_ns = self.wires.now();
                self.phase = Phase::Settling { asked_at_ns };
                self.start_walk()
            }
            Opening::Handshake => self.send_handshake(),
            Opening::EarlyStart => {
                self.start()?;
                self.send_handshake()
            }
        }
    }

    fn send_handshake(&mut self) -> Flow {
        let first = pods_proto::packets::HANDSHAKE.as_slice();
        let sent = self.wires.link(Bearer::Accessory)?.send(first);
        self.phase = Phase::Handshaking {
            handshake: Handshake::default(),
            asked_at_ns: sent.at_ns,
        };
        let request = request_name(Step::AwaitHandshakeAck);
        Ok(self.recorder.sent(sent, first, Mark::Sent, &request)?)
    }

    fn take(&mut self, at_ns: u64, wake: Wake<'_>) -> Flow {
        match wake {
            Wake::Sdu(bearer, sdu) => {
                self.last_read = bearer;
                match bearer {
                    Bearer::Accessory => self.on_packet(at_ns, sdu),
                    Bearer::Att => self.on_pdu(at_ns, sdu),
                }
            }
            Wake::TooLong { bearer, full_len } => {
                self.last_read = bearer;
                Ok(too_long(self.recorder, at_ns, bearer, full_len)?)
            }
            Wake::Idle => Ok(()),
            Wake::Closed(Bearer::Accessory) => Err(EndReason::PeerClosed),
            Wake::Closed(Bearer::Att) => Err(EndReason::AttClosed),
            Wake::Failed(bearer, error) => Err(EndReason::Io(IoFailure::Channel(bearer, error))),
            Wake::WaitFailed(error) => Err(EndReason::Io(IoFailure::Wait(error))),
        }
    }

    fn on_packet(&mut self, at_ns: u64, sdu: &[u8]) -> Flow {
        self.recorder.received(at_ns, sdu)?;
        if matches!(
            self.phase,
            Phase::Handshaking { .. } | Phase::Settling { .. }
        ) {
            self.note_reports(sdu);
        }
        let Phase::Handshaking {
            handshake,
            asked_at_ns,
        } = &mut self.phase
        else {
            return Ok(());
        };
        let Some(request) = handshake.on_packet(sdu) else {
            return Ok(());
        };
        let awaiting = handshake.step();
        let sent = self.wires.link(Bearer::Accessory)?.send(request);
        *asked_at_ns = sent.at_ns;
        self.recorder
            .sent(sent, request, Mark::Sent, &request_name(awaiting))?;
        if awaiting == Step::Ready {
            self.recorder
                .mark(sent.at_ns, Mark::Ready, &"handshake complete")?;
            self.phase = Phase::Settling {
                asked_at_ns: sent.at_ns,
            };
            // The attribute channel is opened only now: a Mac was seen to finish the handshake first.
            if self.open_att.is_some() {
                self.connect_att()?;
                // Nothing was read while the connect lasted, so the wait for the reports counts from its end.
                let asked_at_ns = self.wires.now();
                self.phase = Phase::Settling { asked_at_ns };
            }
            if self.script.walks() {
                self.start_walk()?;
            }
        }
        Ok(())
    }

    /// No packet stamp is taken while the connect lasts, so the two marks bound the stamps that came late.
    fn connect_att(&mut self) -> Flow {
        let what = Bearer::Att.name();
        let open = self.open_att.ok_or(IoFailure::NotGiven { what })?;
        let at_ns = self.wires.now();
        self.recorder.mark(at_ns, Mark::AttConnect, &"connecting")?;
        let channel = open().map_err(EndReason::AttConnect)?;
        let timed = channel.set_recv_timeout(Duration::from_millis(RECV_TIMEOUT_MS));
        timed.map_err(|error| IoFailure::Channel(Bearer::Att, error))?;
        self.wires.att.replace(Some(channel));
        let at_ns = self.wires.now();
        let text = "attribute channel open";
        self.recorder.mark(at_ns, Mark::AttConnect, &text)?;
        // A signal that arrived during the connect ends the run before anything is sent on the channel.
        self.check_interrupted()
    }

    /// Before the gate, remembers an ear detection packet and the microphone side the pods report.
    fn note_reports(&mut self, sdu: &[u8]) {
        if EarReport::parse(sdu).is_ok() {
            self.ear_seen = true;
        }
        if let Ok(command) = ControlCommand::parse(sdu)
            && command.id == ControlId::MIC_SIDE
        {
            self.initial_side = Some(command);
        }
    }

    fn advance(&mut self, now_ns: u64) -> Flow {
        self.end_overdue_wait(now_ns)?;
        match &self.phase {
            Phase::Handshaking {
                handshake,
                asked_at_ns,
            } => {
                if now_ns >= asked_at_ns.saturating_add(self.timing.handshake_step) {
                    return Err(EndReason::HandshakeTimeout(handshake.step()));
                }
                Ok(())
            }
            Phase::Settling { asked_at_ns } => self.settle(now_ns, *asked_at_ns),
            Phase::Scripted { .. } => self.fire_due(now_ns),
            Phase::Held { next, offset_ns } => self.go_on_at_a_line(*next, *offset_ns),
        }
    }

    /// Goes on with the script once a line has arrived on standard input.
    fn go_on_at_a_line(&mut self, next: usize, offset_ns: u64) -> Flow {
        if !answer::line_arrived().map_err(EndReason::Cue)? {
            return Ok(());
        }
        // A stamp taken after the line was read: the waits after the cue count from it, so none ends early.
        let now_ns = self.wires.now();
        self.recorder.mark(now_ns, Mark::Answer, &"")?;
        let gate_ns = now_ns.saturating_sub(offset_ns);
        self.phase = Phase::Scripted { gate_ns, next };
        self.fire_due(now_ns)
    }

    fn settle(&mut self, now_ns: u64, asked_at_ns: u64) -> Flow {
        // The gate also waits for the walk of the table and, in a map, for the declarations.
        if self.waits_on_the_table() {
            return Ok(());
        }
        if self.script.opening == Opening::AttMap {
            return self.open_gate("map");
        }
        if self.script.needs_initial_side && self.initial_side.is_none() {
            if now_ns >= asked_at_ns.saturating_add(self.timing.side_report) {
                return Err(EndReason::NoInitialSide);
            }
            return Ok(());
        }
        if self.ear_seen {
            self.open_gate("ear")
        } else if now_ns >= asked_at_ns.saturating_add(self.timing.ear_report) {
            self.open_gate("timeout")
        } else {
            Ok(())
        }
    }

    fn open_gate(&mut self, why: &str) -> Flow {
        // A fresh stamp, not that of the packet that woke the turn: the walk that ends in the same turn has marked after it, and no mark may go back.
        let gate_ns = self.wires.now();
        self.recorder.mark(gate_ns, Mark::Gate, &why)?;
        self.phase = Phase::Scripted { gate_ns, next: 0 };
        self.fire_due(gate_ns)
    }

    fn fire_due(&mut self, now_ns: u64) -> Flow {
        let script = self.script;
        loop {
            let Phase::Scripted { gate_ns, next } = &mut self.phase else {
                return Ok(());
            };
            let Some(event) = script.events.get(*next) else {
                return Ok(());
            };
            let offset_ns = u64::from(event.at_ms).saturating_mul(NS_PER_MS);
            if now_ns < gate_ns.saturating_add(offset_ns) {
                return Ok(());
            }
            *next = next.saturating_add(1);
            let after = *next;
            self.fire(&event.action)?;
            if matches!(event.action, Action::Hold(_)) {
                let next = after;
                self.phase = Phase::Held { next, offset_ns };
                return Ok(());
            }
        }
    }

    fn fire(&mut self, action: &Action) -> Flow {
        match action {
            Action::Start => self.start(),
            Action::StartCodec(start) => self.start_with(&start.packet(), start),
            Action::Stop => {
                // A script may stop an uplink this run did not start, as the rescue does.
                let (link, dropped) = (self.wires.link(Bearer::Accessory)?, self.dropped);
                drop(
                    self.uplink
                        .take()
                        .unwrap_or_else(|| Uplink { link, dropped }),
                );
                Ok(record_dropped(self.recorder, self.dropped)?)
            }
            Action::Cue(text) => Ok(self.recorder.cue(self.wires.now(), text)?),
            Action::Hold(text) => {
                answer::discard_typed_ahead();
                Ok(self.recorder.cue(self.wires.now(), text)?)
            }
            Action::SetSide(side) => self.set_side(*side),
            Action::RestoreSide => {
                self.side = None;
                Ok(record_dropped(self.recorder, self.dropped)?)
            }
            Action::MarkActive => self.mark_active(),
            Action::MarkIdle => self.mark_idle(),
            Action::DictationStart => self.dictation_start(),
            Action::DictationStop => self.dictation_stop(),
            Action::Step { line, packet } => self.send_step(*line, packet),
            Action::End => Err(EndReason::ScriptDone),
        }
    }

    /// Sends a step as it is and marks it with its line, as `line 4 write-request 0x000e`; its answer is logged like any packet.
    fn send_step(&mut self, line: usize, step: &StepPacket) -> Flow {
        let (bearer, packet) = match step {
            StepPacket::Att(att) => (Bearer::Att, att.pdu()),
            StepPacket::Control(packet) => (Bearer::Accessory, packet.clone()),
        };
        let sent = self.wires.link(bearer)?.send(&packet);
        let text = format_args!("line {line} {step}");
        Ok(self.recorder.sent(sent, &packet, Mark::Step, &text)?)
    }

    fn start(&mut self) -> Flow {
        self.start_with(&uplink::START, &"uplink start")
    }

    fn start_with(&mut self, packet: &[u8], text: &dyn fmt::Display) -> Flow {
        let link = self.wires.link(Bearer::Accessory)?;
        let dropped = self.dropped;
        // The guard exists before the send, so a START that failed half way still gets its STOP; a START while the uplink runs goes out under the guard of the first.
        self.uplink.get_or_insert_with(|| Uplink { link, dropped });
        let sent = link.send(packet);
        if sent.result.is_ok() {
            self.recorder.starts = self.recorder.starts.saturating_add(1);
            self.recorder.uplink_running = true;
        }
        Ok(self.recorder.sent(sent, packet, Mark::Start, text)?)
    }

    fn set_side(&mut self, side: MicSide) -> Flow {
        // A setting is changed only once its original value is known, whatever the script says it needs.
        let Some(original) = self.initial_side else {
            return Err(EndReason::NoInitialSide);
        };
        let link = self.wires.link(Bearer::Accessory)?;
        if self.side.is_none() {
            self.side = Some(SideChange {
                link,
                original,
                dropped: self.dropped,
            });
        }
        let frame = side.command().encode();
        let sent = link.send(&frame);
        let text = format_args!("set {}", side_name(side));
        Ok(self.recorder.sent(sent, &frame, Mark::Side, &text)?)
    }
}

fn too_long(
    recorder: &mut Recorder<'_>,
    at_ns: u64,
    bearer: Bearer,
    full_len: usize,
) -> Result<(), IoFailure> {
    if bearer == Bearer::Accessory {
        recorder.largest_sdu = recorder.largest_sdu.max(full_len);
    }
    let on = bearer.suffix();
    let why = format_args!("longer than the {RECV_BUFFER_LEN} byte receive buffer{on}");
    recorder.unlogged(at_ns, full_len, &why)
}

/// The request that was sent when the handshake began to wait at `awaiting`.
fn request_name(awaiting: Step) -> &'static str {
    match awaiting {
        Step::AwaitHandshakeAck => "handshake",
        Step::AwaitFeaturesAck => "set specific features",
        Step::Ready => "request notifications",
    }
}

fn side_name(side: MicSide) -> &'static str {
    match side {
        MicSide::Automatic => "automatic",
        MicSide::Right => "right",
        MicSide::Left => "left",
    }
}

/// In the order the guards drop; each is recorded even when one before it failed.
fn record_dropped(recorder: &mut Recorder<'_>, dropped: &Dropped) -> Result<(), IoFailure> {
    let stop = dropped.stop.take().map_or(Ok(()), |sent| {
        if sent.result.is_ok() {
            recorder.stops = recorder.stops.saturating_add(1);
            recorder.uplink_running = false;
        }
        recorder.sent(sent, &uplink::STOP, Mark::Stop, &"uplink stop")
    });
    let dictation_stop = dropped.dictation_stop.take().map_or(Ok(()), |sent| {
        if sent.result.is_ok() {
            recorder.dictation_stops = recorder.dictation_stops.saturating_add(1);
        }
        let stop = dictate::STOP;
        recorder.sent(sent, &stop, Mark::DictationStop, &"dictation stop")
    });
    let idle = dropped.idle.take().map_or(Ok(()), |sent| {
        let idle = dictation::SESSION_IDLE;
        recorder.sent(sent, &idle, Mark::Idle, &"session idle")
    });
    let restore = dropped.restore.take().map_or(Ok(()), |(original, sent)| {
        recorder.sent(sent, &original.encode(), Mark::Side, &"restored")
    });
    stop.and(dictation_stop).and(idle).and(restore)
}

/// Marks a reply of the stream with its delay, as the loop does.
fn keep(
    recorder: &mut Recorder<'_>,
    at_ns: u64,
    wake: Wake<'_>,
    start_sent_ns: Option<u64>,
    stop_sent_ns: Option<u64>,
) -> Result<(), IoFailure> {
    match wake {
        Wake::Sdu(Bearer::Accessory, sdu) => recorder.received(at_ns, sdu),
        Wake::Sdu(Bearer::Att, pdu) => {
            recorder.received_pdu(at_ns, pdu)?;
            att::mark_status(recorder, at_ns, pdu, start_sent_ns, stop_sent_ns)
        }
        Wake::TooLong { bearer, full_len } => too_long(recorder, at_ns, bearer, full_len),
        Wake::Idle | Wake::Closed(_) | Wake::Failed(..) | Wake::WaitFailed(_) => Ok(()),
    }
}

/// A run that reached its end still fails when a later write or send did; an earlier reason stands.
fn first_failure(end: EndReason, later: Result<(), IoFailure>) -> EndReason {
    match (end, later) {
        (EndReason::ScriptDone, Err(failure)) => EndReason::Io(failure),
        (end, Ok(()) | Err(_)) => end,
    }
}

/// Opens the control channel and reports the kernel's MTUs for the capture.
pub(super) fn connect_control(
    peer: BdAddr,
    timeout: Duration,
    imtu: Option<u16>,
) -> Result<Channel, ChannelError> {
    let channel = match imtu {
        Some(imtu) => l2cap::connect_with_imtu(peer, CONTROL_PSM, timeout, imtu),
        None => l2cap::connect(peer, CONTROL_PSM, timeout),
    }?;
    let mtus = channel.mtus()?;
    crate::report(&format!(
        "xtask probe: control channel MTU incoming {} bytes, outgoing {} bytes",
        mtus.incoming, mtus.outgoing
    ));
    Ok(channel)
}

pub(crate) fn run(
    links: &Links<'_>,
    script: &Script,
    out: &mut Outputs<'_>,
    interrupted: &AtomicBool,
    now_ns: &dyn Fn() -> u64,
) -> Outcome {
    run_with(links, script, out, interrupted, now_ns, &Timing::REAL)
}

fn run_with(
    links: &Links<'_>,
    script: &Script,
    out: &mut Outputs<'_>,
    interrupted: &AtomicBool,
    now_ns: &dyn Fn() -> u64,
    timing: &Timing,
) -> Outcome {
    let att = RefCell::new(None);
    let wires = Wires {
        accessory: links.accessory,
        att: &att,
        now_ns,
    };
    let dropped = Dropped::default();
    // Declared before the engine, so a panic in the loop still keeps the channels open after the engine's guards have sent what they send.
    let mut tail = Tail {
        wires,
        length_ns: timing.stop_tail,
        dropped: &dropped,
    };
    let mut recorder = Recorder::new(out);
    let (end, start_sent_ns) = {
        let mut engine = Engine {
            wires,
            open_att: links.att,
            script,
            recorder: &mut recorder,
            interrupted,
            timing,
            dropped: &dropped,
            phase: Phase::Handshaking {
                handshake: Handshake::default(),
                asked_at_ns: wires.now(),
            },
            ear_seen: false,
            initial_side: None,
            last_read: Bearer::Att,
            asked: Asked::Nothing,
            checked: false,
            start_sent_ns: None,
            uplink: None,
            dictation: None,
            session: None,
            side: None,
        };
        let end = engine.drive();
        (end, engine.start_sent_ns)
    };
    let end = first_failure(end, record_dropped(&mut recorder, &dropped));
    let stop_sent_ns = dropped.dictation_stop_ns.get();
    let mut kept = Ok(());
    tail.read(&mut |at_ns, wake| {
        // A log that failed once is not written again, and the channels still stay open for the tail.
        if kept.is_ok() {
            kept = keep(&mut recorder, at_ns, wake, start_sent_ns, stop_sent_ns);
        }
    });
    let end = first_failure(end, kept);
    let marked = recorder.mark(wires.now(), Mark::End, &end);
    let end = first_failure(end, marked);
    Outcome {
        sdus_in: recorder.sdus_in,
        largest_sdu: recorder.largest_sdu,
        audio_sdus: recorder.audio_sdus,
        starts: recorder.starts,
        stops: recorder.stops,
        uplink_running: recorder.uplink_running,
        truncated: recorder.truncated,
        att_pdus_in: recorder.att_pdus_in,
        audio_notifications: recorder.audio_notifications,
        dictation_starts: recorder.dictation_starts,
        dictation_stops: recorder.dictation_stops,
        att_requests_refused: recorder.att_requests_refused,
        end,
    }
}

#[cfg(test)]
mod tests;
