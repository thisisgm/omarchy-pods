//! What a run writes: the packet log, the marks and the cues, each line in one write.

use std::fmt;
use std::io::{self, Write};

use pods_proto::dictation::DictationPacket;
use pods_proto::sdulog::{self, Direction};
use pods_proto::uplink::UplinkPacket;
use pods_sys::l2cap::ChannelError;

/// The packet log of a run.
pub(crate) const LOG_NAME: &str = "sdu.log";
/// The log of the attribute channel of a run, in the lines of the packet log.
pub(crate) const ATT_LOG_NAME: &str = "att.log";
/// What the pods listed of their attribute table in a run that walked it.
pub(crate) const HANDLES_NAME: &str = "handles.tsv";
/// The marks of a run: one line for each thing the loop did.
pub(crate) const MARKS_NAME: &str = "marks.tsv";
/// What the cue writer is called when it fails; the command gives it the standard output.
const CUES_NAME: &str = "the cue output";

/// One of the two channels a run may hold open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bearer {
    /// The control channel, whose packets go to the packet log.
    Accessory,
    /// The attribute channel, whose packets go to its own log.
    Att,
}

impl Bearer {
    pub(super) fn other(self) -> Self {
        match self {
            Self::Accessory => Self::Att,
            Self::Att => Self::Accessory,
        }
    }

    /// What a failure calls the channel.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Accessory => "control channel",
            Self::Att => "attribute channel",
        }
    }

    /// What a line about a packet or a failure of this channel ends with; lines of the control channel read as they did when it was the only one.
    pub(super) fn suffix(self) -> &'static str {
        match self {
            Self::Accessory => "",
            Self::Att => ", on the attribute channel",
        }
    }
}

/// The step that failed and why; neither names the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum IoFailure {
    #[error("{1}{on}", on = .0.suffix())]
    Channel(Bearer, ChannelError),
    /// The wait over the open channels failed, so no channel is to blame.
    #[error("{0}")]
    Wait(ChannelError),
    #[error("could not write {file}: {kind}")]
    Write {
        file: &'static str,
        kind: io::ErrorKind,
    },
    /// The script needs a channel or a file its caller did not open.
    #[error("the run was given no {what}")]
    NotGiven { what: &'static str },
}

/// Where a run writes: the packet log, the log of the attribute channel when the run opens one, the list of the table when the run walks it, the marks, and the cues shown to the owner.
pub(crate) struct Outputs<'a> {
    pub(crate) log: &'a mut dyn Write,
    pub(crate) att_log: Option<&'a mut dyn Write>,
    pub(crate) handles: Option<&'a mut dyn Write>,
    pub(crate) marks: &'a mut dyn Write,
    pub(crate) cues: &'a mut dyn Write,
}

/// The second field of a marks line.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Mark {
    Connect,
    AttConnect,
    /// The run closed the attribute channel itself.
    AttClose,
    Sent,
    Ready,
    Gate,
    Start,
    Stop,
    Cue,
    Side,
    Truncated,
    /// What the walk of the attribute table found.
    Handles,
    Active,
    Idle,
    DictationStart,
    DictationStop,
    /// The pods' reply to the start write.
    Started,
    /// The pods' reply to the stop write.
    Stopped,
    /// A request the pods made of the host.
    AttRequest,
    End,
}

impl Mark {
    fn name(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::AttConnect => "att-connect",
            Self::AttClose => "att-close",
            Self::Sent => "sent",
            Self::Ready => "ready",
            Self::Gate => "gate",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Cue => "cue",
            Self::Side => "side",
            Self::Truncated => "truncated",
            Self::Handles => "handles",
            Self::Active => "active",
            Self::Idle => "idle",
            Self::DictationStart => "dictation-start",
            Self::DictationStop => "dictation-stop",
            Self::Started => "started",
            Self::Stopped => "stopped",
            Self::AttRequest => "att-request",
            Self::End => "end",
        }
    }
}

/// One marks line: `1203456789\tgate\tear\n`, the stamp in nanoseconds, the kind and a text.
pub(crate) fn mark_line(at_ns: u64, kind: Mark, text: &dyn fmt::Display) -> String {
    format!("{at_ns}\t{kind}\t{text}\n", kind = kind.name())
}

/// Writes one marks line in a single write, so a line is whole or absent.
pub(crate) fn write_mark(
    marks: &mut dyn Write,
    at_ns: u64,
    kind: Mark,
    text: &dyn fmt::Display,
) -> Result<(), IoFailure> {
    let line = mark_line(at_ns, kind, text);
    let written = marks.write_all(line.as_bytes());
    written.map_err(|error| write_failure(MARKS_NAME, &error))
}

fn write_failure(file: &'static str, error: &io::Error) -> IoFailure {
    IoFailure::Write {
        file,
        kind: error.kind(),
    }
}

/// The channel a packet was sent on, when, and whether it left.
#[derive(Debug, Clone, Copy)]
pub(super) struct Sent {
    pub(super) bearer: Bearer,
    pub(super) at_ns: u64,
    pub(super) result: Result<(), ChannelError>,
}

/// Writes the outputs of a run and counts what went into them.
pub(super) struct Recorder<'o> {
    log: &'o mut dyn Write,
    att_log: Option<&'o mut dyn Write>,
    handles: Option<&'o mut dyn Write>,
    marks: &'o mut dyn Write,
    cues: &'o mut dyn Write,
    pub(super) sdus_in: u64,
    pub(super) audio_sdus: u64,
    pub(super) starts: u32,
    pub(super) stops: u32,
    pub(super) truncated: u32,
    pub(super) att_pdus_in: u64,
    /// Those of them that are audio notifications of the dictation stream.
    pub(super) audio_notifications: u64,
    /// Dictation start writes that left.
    pub(super) dictation_starts: u32,
    /// Dictation stop writes that left.
    pub(super) dictation_stops: u32,
    /// Requests of the pods that were answered with a refusal.
    pub(super) att_requests_refused: u32,
}

impl<'o> Recorder<'o> {
    pub(super) fn new(out: &'o mut Outputs<'_>) -> Self {
        let att_log: Option<&'o mut dyn Write> = match &mut out.att_log {
            Some(log) => Some(&mut **log),
            None => None,
        };
        let handles: Option<&'o mut dyn Write> = match &mut out.handles {
            Some(handles) => Some(&mut **handles),
            None => None,
        };
        Self {
            log: &mut *out.log,
            att_log,
            handles,
            marks: &mut *out.marks,
            cues: &mut *out.cues,
            sdus_in: 0,
            audio_sdus: 0,
            starts: 0,
            stops: 0,
            truncated: 0,
            att_pdus_in: 0,
            audio_notifications: 0,
            dictation_starts: 0,
            dictation_stops: 0,
            att_requests_refused: 0,
        }
    }

    pub(super) fn mark(
        &mut self,
        at_ns: u64,
        kind: Mark,
        text: &dyn fmt::Display,
    ) -> Result<(), IoFailure> {
        write_mark(self.marks, at_ns, kind, text)
    }

    /// Logs one packet in a single write, in the log of its channel, and says whether it is there; one the log format refuses is marked and counted in its place.
    pub(super) fn packet(
        &mut self,
        at_ns: u64,
        bearer: Bearer,
        direction: Direction,
        sdu: &[u8],
    ) -> Result<bool, IoFailure> {
        match sdulog::format_line(at_ns, direction, sdu) {
            Ok(mut line) => {
                line.push('\n');
                let (log, file) = match bearer {
                    Bearer::Accessory => (&mut *self.log, LOG_NAME),
                    Bearer::Att => {
                        let what = ATT_LOG_NAME;
                        let log = self.att_log.as_deref_mut();
                        (log.ok_or(IoFailure::NotGiven { what })?, ATT_LOG_NAME)
                    }
                };
                let written = log.write_all(line.as_bytes());
                written.map_err(|error| write_failure(file, &error))?;
                Ok(true)
            }
            Err(refusal) => {
                self.unlogged(at_ns, sdu.len(), &refusal)?;
                Ok(false)
            }
        }
    }

    /// Marks and counts a packet that is not in the log, with its full length.
    pub(super) fn unlogged(
        &mut self,
        at_ns: u64,
        len: usize,
        why: &dyn fmt::Display,
    ) -> Result<(), IoFailure> {
        self.mark(at_ns, Mark::Truncated, &format_args!("{len} bytes: {why}"))?;
        self.truncated = self.truncated.saturating_add(1);
        Ok(())
    }

    /// Logs a packet from the pods and counts it only once its line is in the log.
    pub(super) fn received(&mut self, at_ns: u64, sdu: &[u8]) -> Result<(), IoFailure> {
        if !self.packet(at_ns, Bearer::Accessory, Direction::FromPods, sdu)? {
            return Ok(());
        }
        self.sdus_in = self.sdus_in.saturating_add(1);
        if matches!(UplinkPacket::parse(sdu), Ok(Some(UplinkPacket::Audio(_)))) {
            self.audio_sdus = self.audio_sdus.saturating_add(1);
        }
        Ok(())
    }

    /// Logs a packet of the attribute channel from the pods and counts it only once its line is in the log.
    pub(super) fn received_pdu(&mut self, at_ns: u64, pdu: &[u8]) -> Result<(), IoFailure> {
        if !self.packet(at_ns, Bearer::Att, Direction::FromPods, pdu)? {
            return Ok(());
        }
        self.att_pdus_in = self.att_pdus_in.saturating_add(1);
        if matches!(
            DictationPacket::parse(pdu),
            Ok(Some(DictationPacket::Audio(_)))
        ) {
            self.audio_notifications = self.audio_notifications.saturating_add(1);
        }
        Ok(())
    }

    /// Writes one line of the list of the pods' attribute table in a single write, as `attribute\t0x0007\t0x2803`.
    pub(super) fn handle_line(&mut self, line: &dyn fmt::Display) -> Result<(), IoFailure> {
        let what = HANDLES_NAME;
        let handles = self.handles.as_deref_mut();
        let handles = handles.ok_or(IoFailure::NotGiven { what })?;
        let written = handles.write_all(format!("{line}\n").as_bytes());
        written.map_err(|error| write_failure(HANDLES_NAME, &error))
    }

    /// Records a packet the host sent, or that it did not leave; the log line and the mark are both tried.
    pub(super) fn sent(
        &mut self,
        sent: Sent,
        sdu: &[u8],
        kind: Mark,
        text: &dyn fmt::Display,
    ) -> Result<(), IoFailure> {
        match sent.result {
            Ok(()) => {
                let logged = self.packet(sent.at_ns, sent.bearer, Direction::ToPods, sdu);
                logged.and(self.mark(sent.at_ns, kind, text))
            }
            Err(error) => {
                let failed = format_args!("{text} failed: {error}");
                let marked = self.mark(sent.at_ns, kind, &failed);
                marked.and(Err(IoFailure::Channel(sent.bearer, error)))
            }
        }
    }

    /// Shows one cue to the owner and marks it.
    pub(super) fn cue(&mut self, at_ns: u64, text: &str) -> Result<(), IoFailure> {
        let shown = self.cues.write_all(format!("cue {text}\n").as_bytes());
        let shown = shown.and_then(|()| self.cues.flush());
        let shown = shown.map_err(|error| write_failure(CUES_NAME, &error));
        shown.and(self.mark(at_ns, Mark::Cue, &text))
    }
}
