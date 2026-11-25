//! A connection's timing marks, one row for each trial of a sitting: read from a trace, the daemon's journal and a log of the sinks.

mod connect;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the takeover command tests also build a playing link"
)]
mod fixture;
mod journal;
mod sinks;
mod takeover;

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, Subcommand};
use pods_core::link::ladder::Attempt;
use pods_core::link::{Phase, STATE_POPULATED_LINE, phase_change_line};
use pods_proto::addr::BdAddr;

#[cfg(test)]
use super::hci::fixture as hci_fixture;
use super::hci::{self, Cut, MAX_TRACE_BYTES, TraceError};
use super::store::{self, MAX_READ_BYTES, StoreError};
use connect::{Began, Connection, connection};
use journal::line_us;
use sinks::first_bluez_sink_us;
use takeover::TakeoverArgs;

const MICROS_PER_SECOND: u64 = 1_000_000;
const MICROS_PER_MILLI: i128 = 1_000;
const NANOS_PER_MICRO: u64 = 1_000;
const INCOMPLETE: &str = "incomplete";
const REASONLESS: &str = "reasonless";
/// How many digits `date +%N` prints.
const NANOS_DIGITS: usize = 9;
/// The word between the stamp and the number of a trials log line.
const TRIAL_WORD: &str = "trial";
/// The header: the trial, what `row` gives of its connection, then the marks of the journal and of the sinks log.
const HEADER: &str = "trial\tbegan\tadvert_ms\tset_configurations\topen_ms\tstart_ms\tcontrol_dials\tmedia_ms\tended\tup_ms\tpopulated_ms\tsink_ms";

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub(crate) struct MarksArgs {
    #[command(flatten)]
    connection: Option<ConnectionArgs>,
    #[command(subcommand)]
    command: Option<MarksCommand>,
}

/// The files of one sitting.
#[derive(Debug, Args)]
struct ConnectionArgs {
    /// The trace btmon -w wrote: the caller's own, with no access for group or others, as every file below.
    trace: PathBuf,
    /// A file of mode 0600 that holds the address of the pods and nothing else; the address is never printed.
    #[arg(long)]
    device_file: PathBuf,
    /// The trials: one line as each starts, the output of date +%s.%N, then the word trial and its number.
    #[arg(long)]
    trials: PathBuf,
    /// The daemon's lines as journalctl -o json wrote them.
    #[arg(long)]
    journal: PathBuf,
    /// The sinks: lines of pactl list short sinks, each after the output of date +%s.%N and a space.
    #[arg(long)]
    sinks: PathBuf,
}

#[derive(Debug, Subcommand)]
enum MarksCommand {
    /// Prints audio source reports and, with a trace, each trial's suspend, close and restart marks.
    Takeover(TakeoverArgs),
}

/// The stretch of a sitting that belongs to one trial: from its line in the trials log to the next trial's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) trial: u32,
    /// Microseconds since 1970, as every time of the marks.
    pub(crate) from_us: u64,
    /// The first moment that is no longer the trial's.
    pub(crate) to_us: u64,
}

impl Window {
    fn holds(&self, at_us: u64) -> bool {
        (self.from_us..self.to_us).contains(&at_us)
    }
}

/// Why no table was printed; none holds the device's address or a byte of a file.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MarksError {
    #[error("provide the connection files or choose the takeover subcommand")]
    NoMarksInput,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Trace(#[from] TraceError),
    #[error("{} is not UTF-8 text after byte {valid_up_to}", .path.display())]
    NotText { path: PathBuf, valid_up_to: usize },
    #[error(
        "trials log line {line} is not the output of date +%s.%N, the word {TRIAL_WORD} and a number, as 1732480000.123456789 {TRIAL_WORD} 3"
    )]
    TrialsLine { line: usize },
    #[error("trials log line {line} is stamped before the line above it")]
    TrialsOrder { line: usize },
    #[error("trials log holds no trial")]
    NoTrial,
    #[error(transparent)]
    SduLog(#[from] super::analyse::input::InputError),
    #[error("meta.txt has no realtime_at_zero_ns for the SDU log")]
    NoSduClock,
    #[error("sdu.log stamp falls outside the supported clock range")]
    SduClockOverflow,
    #[error(
        "trial {trial}: the trace holds the connection of the named device and neither the request nor the command that began it"
    )]
    NoBeginning { trial: u32 },
}

/// A number written in digits alone; `parse` by itself also takes a sign.
fn digits<T: std::str::FromStr>(text: &str) -> Option<T> {
    let plain = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    text.parse().ok().filter(|_| plain)
}

/// Microseconds since 1970.
// Sample input: `1732480000.123456789`, as `date +%s.%N` prints it: seconds since 1970, a dot, nine digits of nanoseconds.
fn stamp_us(stamp: &str) -> Option<u64> {
    let (seconds, nanos) = stamp.split_once('.')?;
    if nanos.len() != NANOS_DIGITS {
        return None;
    }
    let micros = digits::<u64>(nanos)?.checked_div(NANOS_PER_MICRO)?;
    digits::<u64>(seconds)?
        .checked_mul(MICROS_PER_SECOND)?
        .checked_add(micros)
}

/// The number of a trial and when it starts.
// Sample input: `1732480000.123456789 trial 3`.
fn trial_line(line: &str) -> Option<(u32, u64)> {
    let mut words = line.split(' ');
    let (stamp, word, number) = (words.next()?, words.next()?, words.next()?);
    if word != TRIAL_WORD || words.next().is_some() {
        return None;
    }
    Some((digits(number)?, stamp_us(stamp)?))
}

/// One window for each line, in the log's order; the last trial runs to the end of time.
// Sample input: `1732480000.123456789 trial 3`, one line as each trial starts.
pub(crate) fn windows(trials_log: &str) -> Result<Vec<Window>, MarksError> {
    let mut starts: Vec<(u32, u64)> = Vec::new();
    for (line, text) in (1_usize..).zip(trials_log.lines()) {
        let (trial, from_us) = trial_line(text).ok_or(MarksError::TrialsLine { line })?;
        if starts.last().is_some_and(|(_, before)| from_us < *before) {
            return Err(MarksError::TrialsOrder { line });
        }
        starts.push((trial, from_us));
    }
    if starts.is_empty() {
        return Err(MarksError::NoTrial);
    }
    let ends = starts.iter().skip(1).map(|(_, from_us)| *from_us);
    let ends = ends.chain([u64::MAX]);
    let window = |(&(trial, from_us), to_us)| Window {
        trial,
        from_us,
        to_us,
    };
    Ok(starts.iter().zip(ends).map(window).collect())
}

/// Whole milliseconds from the moment the ACL link came up to `mark_us`, below zero for a mark before it; empty for no mark.
fn after(connection: &Connection, mark_us: Option<u64>) -> String {
    let ms = |mark_us: u64| {
        let micros = i128::from(mark_us).saturating_sub(i128::from(connection.acl_us));
        micros.div_euclid(MICROS_PER_MILLI).to_string()
    };
    mark_us.map_or_default(ms)
}

/// The fields of one connection for `HEADER` after the trial, separated by tabs. Times are whole milliseconds after the ACL link came up and absent marks are empty. The `ended` column holds the hex reason, `reasonless` when the reason byte is missing, or `incomplete` when a cut trace leaves the end unknown.
// Sample output: `pods\t-280\t1\t220\t320\t20:0x0000\t420\t0x13\t40\t70\t620`; each control dial is its time, a colon and its result.
pub(crate) fn row(
    connection: &Connection,
    incomplete: bool,
    up_us: Option<u64>,
    populated_us: Option<u64>,
    sink_us: Option<u64>,
) -> String {
    let began = match connection.began {
        Began::Pods => "pods",
        Began::Host => "host",
    };
    let dial =
        |(at_us, result): &(u64, u16)| format!("{}:{result:#06x}", after(connection, Some(*at_us)));
    let dials: Vec<String> = connection.control_dials.iter().map(dial).collect();
    let ended = match connection.ended {
        Some(end) => end
            .reason
            .map_or_else(|| REASONLESS.to_owned(), |reason| format!("{reason:#04x}")),
        None if incomplete => INCOMPLETE.to_owned(),
        None => String::new(),
    };
    [
        began.to_owned(),
        after(connection, connection.advert_us),
        connection.set_configurations.to_string(),
        after(connection, connection.open_us),
        after(connection, connection.start_us),
        dials.join(","),
        after(connection, connection.media_us),
        ended,
        after(connection, up_us),
        after(connection, populated_us),
        after(connection, sink_us),
    ]
    .join("\t")
}

/// The line the daemon logs when its link to the pods comes up.
fn up_line() -> String {
    let features = Phase::AwaitFeaturesAck {
        attempt: Attempt::Held(1),
        until: Duration::ZERO,
    };
    phase_change_line(features, Phase::Up { rerequest: None })
}

fn trace_extent(trace: &[u8]) -> Result<(Option<Cut>, Option<u64>), TraceError> {
    let mut last_record_us = None;
    let cut = hci::walk(trace, |raw| {
        last_record_us = Some(raw.at_ns / NANOS_PER_MICRO);
    })?;
    Ok((cut, last_record_us))
}

/// The table and any trace damage. A cut before a window ends leaves its trial incomplete. An uncut trace is the whole capture.
pub(crate) fn table(
    trace: &[u8],
    device: BdAddr,
    trials_log: &str,
    journal: &str,
    sinks_log: &str,
) -> Result<(String, Option<Cut>), MarksError> {
    let (cut, last_record_us) = trace_extent(trace)?;
    let up = up_line();
    // A trial with no connection keeps the tabs between the fields after its number.
    let no_marks = "\t".repeat(HEADER.matches('\t').count().saturating_sub(1));
    let incomplete_marks = format!("{INCOMPLETE}{no_marks}");
    let mut table = format!("{HEADER}\n");
    for window in windows(trials_log)? {
        let incomplete = cut.is_some() && last_record_us.is_none_or(|last| last < window.to_us);
        let marks = connection(trace, device, &window)?.map(|connection| {
            let lifetime = Window {
                from_us: connection.acl_us,
                to_us: connection.ended.map_or(window.to_us, |end| end.at_us),
                ..window
            };
            row(
                &connection,
                incomplete,
                line_us(journal, &up, &lifetime),
                line_us(journal, STATE_POPULATED_LINE, &lifetime),
                first_bluez_sink_us(sinks_log, device, &lifetime),
            )
        });
        let missing = if incomplete {
            &incomplete_marks
        } else {
            &no_marks
        };
        let marks = marks.as_deref().unwrap_or(missing);
        // Writing into a String cannot fail.
        let _written = writeln!(table, "{}\t{marks}", window.trial);
    }
    Ok((table, cut))
}

/// A log read whole, by the rules of a capture.
fn text(path: &Path) -> Result<String, MarksError> {
    let bytes = store::read_trace(path, MAX_READ_BYTES)?;
    String::from_utf8(bytes).map_err(|error| MarksError::NotText {
        path: path.to_path_buf(),
        valid_up_to: error.utf8_error().valid_up_to(),
    })
}

/// Reads the files of one sitting and gives the table of its trials, with the damaged record its trace was read up to.
pub(crate) fn marks(args: &MarksArgs) -> Result<(String, Option<Cut>), MarksError> {
    if let Some(MarksCommand::Takeover(args)) = &args.command {
        return takeover::table(args);
    }
    let args = args.connection.as_ref().ok_or(MarksError::NoMarksInput)?;
    let device = store::read_device_file(&args.device_file)?;
    let trace = store::read_trace(&args.trace, MAX_TRACE_BYTES)?;
    let (trials, journal, sinks) = (
        text(&args.trials)?,
        text(&args.journal)?,
        text(&args.sinks)?,
    );
    table(&trace, device, &trials, &journal, &sinks)
}

#[cfg(test)]
mod tests;
