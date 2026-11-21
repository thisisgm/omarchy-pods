//! Microphone probe commands: `init` creates the private session directory, `capture` records one scripted run into it, and `stop` ends an uplink a killed capture left running.

mod capture;
mod handshake;
mod script;
mod store;

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use pods_proto::addr::BdAddr;
use pods_proto::packets::CONTROL_PSM;
use pods_sys::l2cap::{self, Channel, ChannelError};
use rustix::time::{ClockId, clock_gettime};
use signal_hook::consts::{SIGINT, SIGTERM};

use crate::{print_line, report};
use capture::{EndReason, LOG_NAME, MARKS_NAME, Mark, Outcome, Outputs, STOP_TAIL_MS};
use script::{ScriptError, ScriptName};
use store::{Run, Session, StoreError};

/// Bounds the connect: the connect timeout of aacp.rs in librepods-org/librepods pull request 655.
const CONNECT_TIMEOUT_MS: u64 = 10_000;
/// What a run was recorded with and when its clock began.
const META_NAME: &str = "meta.txt";
/// The `tool=` line of `META_NAME`.
const TOOL: &str = concat!("xtask probe capture ", env!("CARGO_PKG_VERSION"));
/// Nanoseconds in the seconds a clock reading is split into.
const NS_PER_SECOND: u64 = 1_000_000_000;

/// Commands that prepare and record a microphone probe session.
#[derive(Debug, Parser)]
#[command(name = "probe", bin_name = "cargo xtask probe")]
struct Probe {
    #[command(subcommand)]
    command: ProbeCommand,
}

#[derive(Debug, Subcommand)]
enum ProbeCommand {
    /// Creates or checks the session directory under $HOME/omapods-captures and prints its path.
    Init {
        /// The session stamp, from date -u +%Y%m%dT%H%M%SZ.
        #[arg(long)]
        stamp: String,
    },
    /// Records one run of a script into a new run directory of the session; each cue for the owner is a line on stdout.
    Capture(CaptureArgs),
    /// Sends STOP to the pods, for a capture that was killed before it could.
    Stop,
}

#[derive(Debug, Args)]
struct CaptureArgs {
    /// The stamp the session was created with.
    #[arg(long)]
    stamp: String,
    /// A new name for this run: 1 to 32 characters of a-z, 0-9 and -.
    #[arg(long)]
    run: String,
    /// The script to follow; a name that is not one is answered with the list.
    #[arg(long)]
    script: ScriptName,
    /// How many minutes listen and soak run, 1 to 90; no other script takes it.
    #[arg(long)]
    minutes: Option<u32>,
}

/// What a command takes from the machine, so a test can stand in for each part.
struct Host<'a> {
    home: Option<OsString>,
    connect: &'a dyn Fn(BdAddr) -> Result<Channel, ChannelError>,
    stdout: &'a mut dyn Write,
    stderr: &'a mut dyn Write,
}

impl Host<'_> {
    /// One line for the owner.
    fn say(&mut self, message: &dyn fmt::Display) {
        // A standard error that cannot be written leaves nowhere to say so.
        let _unreported = writeln!(self.stderr, "xtask probe: {message}");
    }
}

/// Why a capture or a stop did not get as far as the script; none of them carries the device address.
#[derive(Debug, thiserror::Error)]
enum ProbeError {
    #[error("HOME is not set, and the store lives under it")]
    NoHome,
    #[error(transparent)]
    Script(#[from] ScriptError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("could not watch for SIGINT and SIGTERM: {0}")]
    Signals(io::Error),
    #[error("could not write {file}: {source}")]
    Write {
        file: &'static str,
        source: io::Error,
    },
    #[error("{0}")]
    Connect(ChannelError),
}

/// Runs one probe command; `args` starts with the word `probe`.
pub(crate) fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let command = match Probe::try_parse_from(args) {
        Ok(probe) => probe.command,
        Err(error) => return usage(&error),
    };
    let connect = |peer| {
        let timeout = Duration::from_millis(CONNECT_TIMEOUT_MS);
        l2cap::connect(peer, CONTROL_PSM, timeout)
    };
    let mut host = Host {
        home: env::var_os("HOME"),
        connect: &connect,
        stdout: &mut io::stdout(),
        stderr: &mut io::stderr(),
    };
    match command {
        ProbeCommand::Init { stamp } => init(&stamp),
        ProbeCommand::Capture(args) => match watch_signals() {
            Ok(interrupted) => capture(&args, &mut host, &interrupted),
            Err(error) => {
                host.say(&error);
                ExitCode::FAILURE
            }
        },
        ProbeCommand::Stop => stop(&mut host),
    }
}

/// Opens the session for `stamp` and prints its path, which is all init writes on stdout.
fn init(stamp: &str) -> ExitCode {
    let Some(home) = env::var_os("HOME") else {
        report("xtask probe: HOME is not set, and the store lives under it");
        return ExitCode::FAILURE;
    };
    // Every path printed below is built from HOME, and a lossy print would name a directory that does not exist.
    if home.to_str().is_none() {
        #[expect(
            clippy::unnecessary_debug_formatting,
            reason = "Debug escapes the bytes that are not UTF-8, where Display would replace them"
        )]
        let message = format!(
            "xtask probe: HOME {home:?} is not valid UTF-8, so the session path could not be printed as it is on disk"
        );
        report(&message);
        return ExitCode::FAILURE;
    }
    match Session::open(&store::root(&home), stamp) {
        Ok(session) => {
            print_line(&session.path().display().to_string());
            ExitCode::SUCCESS
        }
        Err(error) => {
            report(&format!("xtask probe: {error}"));
            ExitCode::FAILURE
        }
    }
}

/// Records one run and says how it ended; only a script that ran to its end succeeds, and a raised `interrupted` ends it early.
fn capture(args: &CaptureArgs, host: &mut Host<'_>, interrupted: &AtomicBool) -> ExitCode {
    match record(args, host, interrupted) {
        Ok(outcome) => {
            host.say(&summary(&outcome));
            exit_code(&outcome)
        }
        Err(error) => {
            host.say(&error);
            ExitCode::FAILURE
        }
    }
}

fn record(
    args: &CaptureArgs,
    host: &mut Host<'_>,
    interrupted: &AtomicBool,
) -> Result<Outcome, ProbeError> {
    let script = script::build(args.script, args.minutes)?;
    let root = store::root(host.home.as_deref().ok_or(ProbeError::NoHome)?);
    let session = Session::open(&root, &args.stamp)?;
    // The device is read before the run is created, so a missing device file does not use up a run name.
    let peer = store::read_device(&root)?;
    let run = Run::create(&session, &args.run)?;
    let mut log = run.create_file(LOG_NAME)?;
    let mut marks = run.create_file(MARKS_NAME)?;
    let mut meta = run.create_file(META_NAME)?;
    let zero_ns = clock_ns(ClockId::Monotonic);
    let realtime_at_zero_ns = clock_ns(ClockId::Realtime);
    let now_ns = || clock_ns(ClockId::Monotonic).saturating_sub(zero_ns);
    let script_name = args.script;
    let lines =
        format!("script={script_name}\ntool={TOOL}\nrealtime_at_zero_ns={realtime_at_zero_ns}\n");
    let written = meta.write_all(lines.as_bytes());
    written.map_err(|source| ProbeError::Write {
        file: META_NAME,
        source,
    })?;
    let channel = match (host.connect)(peer) {
        Ok(channel) => channel,
        Err(error) => {
            // The connect error is what the owner is told; the mark only keeps it with the run.
            let _unmarked = capture::write_mark(&mut marks, now_ns(), Mark::End, &error);
            return Err(ProbeError::Connect(error));
        }
    };
    let mut out = Outputs {
        log: &mut log,
        marks: &mut marks,
        cues: &mut *host.stdout,
    };
    Ok(capture::run(
        &channel,
        &script,
        &mut out,
        interrupted,
        &now_ns,
    ))
}

/// Connects, runs the handshake and sends STOP, without a run directory and without a START.
fn stop(host: &mut Host<'_>) -> ExitCode {
    match rescue(host) {
        Ok(outcome) => {
            if outcome.end == EndReason::ScriptDone {
                host.say(&format_args!(
                    "STOP was sent and the channel was kept open for {STOP_TAIL_MS} ms"
                ));
            } else {
                host.say(&format_args!("STOP was not sent: {}", outcome.end));
            }
            exit_code(&outcome)
        }
        Err(error) => {
            host.say(&error);
            ExitCode::FAILURE
        }
    }
}

fn rescue(host: &mut Host<'_>) -> Result<Outcome, ProbeError> {
    let root = store::root(host.home.as_deref().ok_or(ProbeError::NoHome)?);
    let peer = store::read_device(&root)?;
    let channel = (host.connect)(peer).map_err(ProbeError::Connect)?;
    // Nothing of a rescue is kept, and no signal is watched: one that kills it has only cost a STOP that can be sent again.
    let (mut log, mut marks, mut cues) = (io::sink(), io::sink(), io::sink());
    let mut out = Outputs {
        log: &mut log,
        marks: &mut marks,
        cues: &mut cues,
    };
    let never = AtomicBool::new(false);
    let now_ns = || clock_ns(ClockId::Monotonic);
    Ok(capture::run(
        &channel,
        &script::rescue(),
        &mut out,
        &never,
        &now_ns,
    ))
}

fn exit_code(outcome: &Outcome) -> ExitCode {
    if outcome.end == EndReason::ScriptDone {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The one sentence a capture ends with: the reason, whether STOP followed START, and the counts.
fn summary(outcome: &Outcome) -> String {
    let Outcome {
        sdus_in,
        audio_sdus,
        starts,
        stops,
        truncated,
        end,
    } = outcome;
    let counts = format!(
        "packets in {sdus_in}, with audio {audio_sdus}, not logged {truncated}, START sent {starts}, STOP sent {stops}"
    );
    if *end == EndReason::ScriptDone {
        return format!("capture complete: {counts}");
    }
    let stop = if *starts == 0 {
        ""
    } else if stops >= starts {
        "; STOP was sent"
    } else {
        "; STOP was NOT sent, so run cargo xtask probe stop"
    };
    format!("capture ended early: {end}{stop} ({counts})")
}

/// Raises the returned flag on SIGINT and SIGTERM, which the loop then sees within one receive timeout.
fn watch_signals() -> Result<Arc<AtomicBool>, ProbeError> {
    let interrupted = Arc::new(AtomicBool::new(false));
    for signal in [SIGINT, SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&interrupted))
            .map_err(ProbeError::Signals)?;
    }
    Ok(interrupted)
}

/// A clock reading in nanoseconds; a reading before the clock's own zero cannot happen and reads as that zero.
// Sample input: tv_sec 1763730000 and tv_nsec 5, read as 1763730000000000005.
fn clock_ns(clock: ClockId) -> u64 {
    let time = clock_gettime(clock);
    let seconds = u64::try_from(time.tv_sec).unwrap_or(0);
    let nanoseconds = u64::try_from(time.tv_nsec).unwrap_or(0);
    seconds
        .saturating_mul(NS_PER_SECOND)
        .saturating_add(nanoseconds)
}

/// Help goes to stdout and succeeds; a parse error goes to stderr and fails.
fn usage(error: &clap::Error) -> ExitCode {
    let text = error.render().to_string();
    if error.use_stderr() {
        report(text.trim_end());
        ExitCode::FAILURE
    } else {
        print_line(text.trim_end());
        ExitCode::SUCCESS
    }
}
