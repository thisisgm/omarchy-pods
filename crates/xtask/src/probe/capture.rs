//! The capture loop: one thread reads the control channel, stamps and logs every packet, and follows a script.

mod guard;
mod record;

use std::convert::Infallible;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::ear::EarReport;
use pods_proto::uplink::{self, MicSide};
use pods_sys::l2cap::{Channel, ChannelError, Received};
use rustix::io::Errno;

use super::handshake::{Handshake, Step};
use super::script::{Action, Opening, Script};
use guard::{Dropped, SideChange, Tail, Uplink};
pub(crate) use record::{IoFailure, LOG_NAME, MARKS_NAME, Mark, Outputs, write_mark};
use record::{Recorder, Sent};

/// How long one handshake request may go unanswered: daemon/main.cpp asks again after 2 s, the probe fails naming the step.
const HANDSHAKE_STEP_TIMEOUT_MS: u64 = 2_000;
/// How long START waits for an ear detection packet: kEarReportWaitMs in pull request 85 of this repository.
const EAR_REPORT_WAIT_MS: u64 = 3_000;
/// Bounds how late a timed action fires and how late a signal is seen; chosen here.
const RECV_TIMEOUT_MS: u64 = 10;
/// The receive buffer of aacp.rs in librepods-org/librepods pull request 655.
const RECV_BUFFER_LEN: usize = 4096;
/// How long the channel stays open after STOP: pull request 85 of this repository flushes for up to 1 s before it closes.
pub(crate) const STOP_TAIL_MS: u64 = 1_000;
/// How long a script that changes the microphone side waits for the pods to report it; chosen here to match the ear wait.
const SIDE_REPORT_WAIT_MS: u64 = 3_000;
/// Nanoseconds in the milliseconds the waits and the script offsets are written in.
const NS_PER_MS: u64 = 1_000_000;

/// How long the loop waits at each step, in nanoseconds; a test passes shorter waits.
#[derive(Debug, Clone, Copy)]
struct Timing {
    handshake_step: u64,
    ear_report: u64,
    side_report: u64,
    stop_tail: u64,
}

impl Timing {
    const REAL: Self = Self {
        handshake_step: HANDSHAKE_STEP_TIMEOUT_MS * NS_PER_MS,
        ear_report: EAR_REPORT_WAIT_MS * NS_PER_MS,
        side_report: SIDE_REPORT_WAIT_MS * NS_PER_MS,
        stop_tail: STOP_TAIL_MS * NS_PER_MS,
    };
}

/// Why a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndReason {
    ScriptDone,
    Interrupted,
    PeerClosed,
    HandshakeTimeout(Step),
    NoInitialSide,
    Io(IoFailure),
}

impl fmt::Display for EndReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScriptDone => f.write_str("the script ran to its end"),
            Self::Interrupted => f.write_str("interrupted by a signal"),
            Self::PeerClosed => f.write_str("the pods closed the channel"),
            Self::HandshakeTimeout(step) => {
                write!(f, "the handshake timed out waiting for {step}")
            }
            Self::NoInitialSide => f.write_str(
                "the pods did not report their microphone side, so no setting was changed",
            ),
            Self::Io(failure) => write!(f, "{failure}"),
        }
    }
}

impl From<IoFailure> for EndReason {
    fn from(failure: IoFailure) -> Self {
        Self::Io(failure)
    }
}

/// What a run counted and why it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Outcome {
    /// Packets from the pods that went into the log.
    pub(crate) sdus_in: u64,
    /// Those of them that carry coded audio.
    pub(crate) audio_sdus: u64,
    /// START packets that left.
    pub(crate) starts: u32,
    /// STOP packets that left.
    pub(crate) stops: u32,
    /// Packets marked and not logged: longer than the receive buffer, or refused by the log format.
    pub(crate) truncated: u32,
    pub(crate) end: EndReason,
}

/// The channel and the clock, which every sender needs together.
#[derive(Clone, Copy)]
struct Link<'a> {
    channel: &'a Channel,
    now_ns: &'a dyn Fn() -> u64,
}

impl Link<'_> {
    fn now(&self) -> u64 {
        (self.now_ns)()
    }

    /// Sends one packet; the stamp is taken just before the send, so it is when the host asked.
    fn send(&self, sdu: &[u8]) -> Sent {
        let at_ns = self.now();
        Sent {
            at_ns,
            result: self.channel.send(sdu),
        }
    }
}

/// What one wait on the channel ended with.
#[derive(Clone, Copy)]
enum Wake<'b> {
    Sdu(&'b [u8]),
    /// A packet of this many bytes that the buffer could not hold.
    TooLong {
        full_len: usize,
    },
    /// The receive timeout or a signal: nothing arrived.
    Idle,
    Closed,
    Failed(ChannelError),
}

/// One wait on the channel; the stamp is the first statement after the wait returns.
fn receive<'b>(link: Link<'_>, buf: &'b mut [u8]) -> (u64, Wake<'b>) {
    let received = link.channel.recv(buf);
    let at_ns = link.now();
    (at_ns, wake_of(received, buf))
}

/// Reads what a wait returned; a timeout and a signal both mean that nothing arrived.
fn wake_of(received: Result<Received, ChannelError>, buf: &[u8]) -> Wake<'_> {
    match received {
        Ok(Received::Sdu(len)) => match buf.get(..len) {
            Some(sdu) => Wake::Sdu(sdu),
            None => Wake::TooLong { full_len: len },
        },
        Ok(Received::Truncated { full_len }) => Wake::TooLong { full_len },
        Ok(Received::TimedOut) | Err(ChannelError::Recv(Errno::INTR)) => Wake::Idle,
        Ok(Received::Closed) => Wake::Closed,
        Err(error) => Wake::Failed(error),
    }
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
}

/// One run: the loop's state and the two guards, which drop with it on every way out.
struct Engine<'a, 'o> {
    link: Link<'a>,
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
    // Fields drop in this order, so the STOP leaves before the side is put back.
    uplink: Option<Uplink<'a>>,
    side: Option<SideChange<'a>>,
}

impl Engine<'_, '_> {
    fn drive(&mut self) -> EndReason {
        let Err(end) = self.steps();
        end
    }

    fn steps(&mut self) -> Result<Infallible, EndReason> {
        let recv_timeout = Duration::from_millis(RECV_TIMEOUT_MS);
        let timed = self.link.channel.set_recv_timeout(recv_timeout);
        timed.map_err(IoFailure::Channel)?;
        self.recorder
            .mark(self.link.now(), Mark::Connect, &"channel open")?;
        // A signal that arrived during the connect ends the run before anything is sent.
        self.check_interrupted()?;
        self.open()?;
        let mut buf = [0_u8; RECV_BUFFER_LEN];
        loop {
            let (at_ns, wake) = receive(self.link, &mut buf);
            self.take(at_ns, wake)?;
            self.check_interrupted()?;
            self.advance(at_ns)?;
        }
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
            Opening::Knock => self.open_gate(self.link.now(), "connect"),
            Opening::Handshake => self.send_handshake(),
            Opening::EarlyStart => {
                self.start()?;
                self.send_handshake()
            }
        }
    }

    fn send_handshake(&mut self) -> Flow {
        let first = Handshake::first();
        let sent = self.link.send(first);
        self.phase = Phase::Handshaking {
            handshake: Handshake::default(),
            asked_at_ns: sent.at_ns,
        };
        let request = request_name(Step::AwaitHandshakeAck);
        Ok(self.recorder.sent(sent, first, Mark::Sent, &request)?)
    }

    fn take(&mut self, at_ns: u64, wake: Wake<'_>) -> Flow {
        match wake {
            Wake::Sdu(sdu) => self.on_packet(at_ns, sdu),
            Wake::TooLong { full_len } => {
                let why = format_args!("longer than the {RECV_BUFFER_LEN} byte receive buffer");
                Ok(self.recorder.unlogged(at_ns, full_len, &why)?)
            }
            Wake::Idle => Ok(()),
            Wake::Closed => Err(EndReason::PeerClosed),
            Wake::Failed(error) => Err(EndReason::Io(IoFailure::Channel(error))),
        }
    }

    fn on_packet(&mut self, at_ns: u64, sdu: &[u8]) -> Flow {
        self.recorder.received(at_ns, sdu)?;
        if !matches!(self.phase, Phase::Scripted { .. }) {
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
        let sent = self.link.send(request);
        *asked_at_ns = sent.at_ns;
        self.recorder
            .sent(sent, request, Mark::Sent, &request_name(awaiting))?;
        if awaiting == Step::Ready {
            self.recorder
                .mark(sent.at_ns, Mark::Ready, &"handshake complete")?;
            self.phase = Phase::Settling {
                asked_at_ns: sent.at_ns,
            };
        }
        Ok(())
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

    /// Ends a wait that ran out, opens the gate when its conditions hold, and fires the events that are due.
    fn advance(&mut self, now_ns: u64) -> Flow {
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
        }
    }

    fn settle(&mut self, now_ns: u64, asked_at_ns: u64) -> Flow {
        if self.script.needs_initial_side && self.initial_side.is_none() {
            if now_ns >= asked_at_ns.saturating_add(self.timing.side_report) {
                return Err(EndReason::NoInitialSide);
            }
            return Ok(());
        }
        if self.ear_seen {
            self.open_gate(now_ns, "ear")
        } else if now_ns >= asked_at_ns.saturating_add(self.timing.ear_report) {
            self.open_gate(now_ns, "timeout")
        } else {
            Ok(())
        }
    }

    fn open_gate(&mut self, gate_ns: u64, why: &str) -> Flow {
        self.recorder.mark(gate_ns, Mark::Gate, &why)?;
        self.phase = Phase::Scripted { gate_ns, next: 0 };
        self.fire_due(gate_ns)
    }

    fn fire_due(&mut self, now_ns: u64) -> Flow {
        loop {
            let Phase::Scripted { gate_ns, next } = &mut self.phase else {
                return Ok(());
            };
            let Some(event) = self.script.events.get(*next) else {
                return Ok(());
            };
            let offset_ns = u64::from(event.at_ms).saturating_mul(NS_PER_MS);
            if now_ns < gate_ns.saturating_add(offset_ns) {
                return Ok(());
            }
            *next = next.saturating_add(1);
            self.fire(event.action)?;
        }
    }

    fn fire(&mut self, action: Action) -> Flow {
        match action {
            Action::Start => self.start(),
            Action::Stop => {
                // A script may stop an uplink this run did not start, as the rescue does.
                let (link, dropped) = (self.link, self.dropped);
                drop(
                    self.uplink
                        .take()
                        .unwrap_or_else(|| Uplink { link, dropped }),
                );
                Ok(record_dropped(self.recorder, self.dropped)?)
            }
            Action::Cue(text) => Ok(self.recorder.cue(self.link.now(), text)?),
            Action::SetSide(side) => self.set_side(side),
            Action::RestoreSide => {
                self.side = None;
                Ok(record_dropped(self.recorder, self.dropped)?)
            }
            Action::End => Err(EndReason::ScriptDone),
        }
    }

    fn start(&mut self) -> Flow {
        // A second START is not known to be harmless, so a running uplink is left as it is.
        if self.uplink.is_some() {
            return Ok(());
        }
        // The guard exists before the send, so a START that failed half way still gets its STOP.
        self.uplink = Some(Uplink {
            link: self.link,
            dropped: self.dropped,
        });
        let sent = self.link.send(&uplink::START);
        if sent.result.is_ok() {
            self.recorder.starts = self.recorder.starts.saturating_add(1);
        }
        let recorded = self
            .recorder
            .sent(sent, &uplink::START, Mark::Start, &"uplink start");
        Ok(recorded?)
    }

    fn set_side(&mut self, side: MicSide) -> Flow {
        // A setting is changed only once its original value is known, whatever the script says it needs.
        let Some(original) = self.initial_side else {
            return Err(EndReason::NoInitialSide);
        };
        if self.side.is_none() {
            self.side = Some(SideChange {
                link: self.link,
                original,
                dropped: self.dropped,
            });
        }
        let frame = side.command().encode();
        let sent = self.link.send(&frame);
        let text = format_args!("set {}", side_name(side));
        Ok(self.recorder.sent(sent, &frame, Mark::Side, &text)?)
    }
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

/// Records what the guards sent as they dropped; the second is recorded even when the first failed.
fn record_dropped(recorder: &mut Recorder<'_>, dropped: &Dropped) -> Result<(), IoFailure> {
    let stop = dropped.stop.take().map_or(Ok(()), |sent| {
        if sent.result.is_ok() {
            recorder.stops = recorder.stops.saturating_add(1);
        }
        recorder.sent(sent, &uplink::STOP, Mark::Stop, &"uplink stop")
    });
    let restore = dropped.restore.take().map_or(Ok(()), |(original, sent)| {
        recorder.sent(sent, &original.encode(), Mark::Side, &"restored")
    });
    stop.and(restore)
}

/// A run that reached its end still fails when a later write or send did; an earlier reason stands.
fn first_failure(end: EndReason, later: Result<(), IoFailure>) -> EndReason {
    match (end, later) {
        (EndReason::ScriptDone, Err(failure)) => EndReason::Io(failure),
        (end, Ok(()) | Err(_)) => end,
    }
}

/// Follows `script` on an open channel until it ends, the pods hang up, a signal raises `interrupted`, or a step fails.
pub(crate) fn run(
    channel: &Channel,
    script: &Script,
    out: &mut Outputs<'_>,
    interrupted: &AtomicBool,
    now_ns: &dyn Fn() -> u64,
) -> Outcome {
    run_with(channel, script, out, interrupted, now_ns, &Timing::REAL)
}

fn run_with(
    channel: &Channel,
    script: &Script,
    out: &mut Outputs<'_>,
    interrupted: &AtomicBool,
    now_ns: &dyn Fn() -> u64,
    timing: &Timing,
) -> Outcome {
    let link = Link { channel, now_ns };
    let dropped = Dropped::default();
    // Declared before the engine, so it drops after the engine's guards have sent what they send.
    let _tail = Tail {
        link,
        length_ns: timing.stop_tail,
        dropped: &dropped,
    };
    let mut recorder = Recorder::new(out);
    let end = {
        let mut engine = Engine {
            link,
            script,
            recorder: &mut recorder,
            interrupted,
            timing,
            dropped: &dropped,
            phase: Phase::Handshaking {
                handshake: Handshake::default(),
                asked_at_ns: link.now(),
            },
            ear_seen: false,
            initial_side: None,
            uplink: None,
            side: None,
        };
        engine.drive()
    };
    let end = first_failure(end, record_dropped(&mut recorder, &dropped));
    let end = first_failure(end, recorder.mark(link.now(), Mark::End, &end));
    Outcome {
        sdus_in: recorder.sdus_in,
        audio_sdus: recorder.audio_sdus,
        starts: recorder.starts,
        stops: recorder.stops,
        truncated: recorder.truncated,
        end,
    }
}

#[cfg(test)]
mod tests;
