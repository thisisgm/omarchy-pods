//! What a run writes: the packet log, the marks and the cues, each line in one write.

use std::fmt;
use std::io::{self, Write};

use pods_proto::sdulog::{self, Direction};
use pods_proto::uplink::UplinkPacket;
use pods_sys::l2cap::ChannelError;

/// The packet log of a run.
pub(crate) const LOG_NAME: &str = "sdu.log";
/// The marks of a run: one line for each thing the loop did.
pub(crate) const MARKS_NAME: &str = "marks.tsv";
/// What the cue writer is called when it fails; the command gives it the standard output.
const CUES_NAME: &str = "the cue output";

/// The step that failed and why; neither names the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum IoFailure {
    #[error("{0}")]
    Channel(ChannelError),
    #[error("could not write {file}: {kind}")]
    Write {
        file: &'static str,
        kind: io::ErrorKind,
    },
}

/// Where a run writes: the packet log, the marks, and the cues shown to the owner.
pub(crate) struct Outputs<'a> {
    pub(crate) log: &'a mut dyn Write,
    pub(crate) marks: &'a mut dyn Write,
    pub(crate) cues: &'a mut dyn Write,
}

/// The second field of a marks line.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Mark {
    Connect,
    Sent,
    Ready,
    Gate,
    Start,
    Stop,
    Cue,
    Side,
    Truncated,
    End,
}

impl Mark {
    fn name(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Sent => "sent",
            Self::Ready => "ready",
            Self::Gate => "gate",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Cue => "cue",
            Self::Side => "side",
            Self::Truncated => "truncated",
            Self::End => "end",
        }
    }
}

/// Writes one marks line in a single write, so a line is whole or absent.
// Sample output: "1203456789\tgate\tear\n", the stamp in nanoseconds, the kind and a text.
pub(crate) fn write_mark(
    marks: &mut dyn Write,
    at_ns: u64,
    kind: Mark,
    text: &dyn fmt::Display,
) -> Result<(), IoFailure> {
    let line = format!("{at_ns}\t{kind}\t{text}\n", kind = kind.name());
    let written = marks.write_all(line.as_bytes());
    written.map_err(|error| write_failure(MARKS_NAME, &error))
}

fn write_failure(file: &'static str, error: &io::Error) -> IoFailure {
    IoFailure::Write {
        file,
        kind: error.kind(),
    }
}

/// When a packet was sent and whether it left.
#[derive(Debug, Clone, Copy)]
pub(super) struct Sent {
    pub(super) at_ns: u64,
    pub(super) result: Result<(), ChannelError>,
}

/// Writes the three outputs of a run and counts what went into them.
pub(super) struct Recorder<'o> {
    log: &'o mut dyn Write,
    marks: &'o mut dyn Write,
    cues: &'o mut dyn Write,
    pub(super) sdus_in: u64,
    pub(super) audio_sdus: u64,
    pub(super) starts: u32,
    pub(super) stops: u32,
    pub(super) truncated: u32,
}

impl<'o> Recorder<'o> {
    pub(super) fn new(out: &'o mut Outputs<'_>) -> Self {
        Self {
            log: &mut *out.log,
            marks: &mut *out.marks,
            cues: &mut *out.cues,
            sdus_in: 0,
            audio_sdus: 0,
            starts: 0,
            stops: 0,
            truncated: 0,
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

    /// Logs one packet in a single write and says whether it is in the log; one the log format refuses is marked and counted in its place.
    pub(super) fn packet(
        &mut self,
        at_ns: u64,
        direction: Direction,
        sdu: &[u8],
    ) -> Result<bool, IoFailure> {
        match sdulog::format_line(at_ns, direction, sdu) {
            Ok(mut line) => {
                line.push('\n');
                let written = self.log.write_all(line.as_bytes());
                written.map_err(|error| write_failure(LOG_NAME, &error))?;
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
        if !self.packet(at_ns, Direction::FromPods, sdu)? {
            return Ok(());
        }
        self.sdus_in = self.sdus_in.saturating_add(1);
        if matches!(UplinkPacket::parse(sdu), Ok(Some(UplinkPacket::Audio(_)))) {
            self.audio_sdus = self.audio_sdus.saturating_add(1);
        }
        Ok(())
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
                let logged = self.packet(sent.at_ns, Direction::ToPods, sdu);
                logged.and(self.mark(sent.at_ns, kind, text))
            }
            Err(error) => {
                let failed = format_args!("{text} failed: {error}");
                let marked = self.mark(sent.at_ns, kind, &failed);
                marked.and(Err(IoFailure::Channel(error)))
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
