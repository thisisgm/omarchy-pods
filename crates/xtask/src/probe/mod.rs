//! Microphone probe commands: sessions, scripted captures, trace imports and their analysis.

mod a2dp;
mod analyse;
mod audio;
mod bench;
mod capture;
mod decode;
mod decode_opus;
mod dictate;
mod eldfile;
#[cfg(test)]
mod fixture;
mod hci;
mod marks;
pub(crate) mod oggfile;
mod script;
mod service;
mod source;
mod store;
mod wav;
mod xorshift;

use std::env;
use std::ffi::{OsString, c_int};
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use pods_proto::addr::BdAddr;
use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
use pods_sys::l2cap::{self, Channel, ChannelError};
use rustix::time::{ClockId, clock_gettime};
use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};

use crate::hw::unit::INSTALLED;
use crate::{print_line, report};
use analyse::input::meta_text;
pub(crate) use capture::HANDSHAKE_STEP_TIMEOUT_MS;
use capture::{
    ATT_LOG_NAME, EndReason, HANDLES_NAME, LOG_NAME, Links, MARKS_NAME, Mark, OpenAtt, Outcome,
    Outputs, STOP_TAIL_MS,
};
use script::{Opening, Route, Script, ScriptError, ScriptName, StepsError};
use service::{RunLock, ServiceError};
use store::{Run, Session, StoreError};

/// Bounds the connect: the connect timeout of aacp.rs in librepods-org/librepods pull request 655.
pub(crate) const CONNECT_TIMEOUT_MS: u64 = 10_000;
/// What a run was recorded with and when its clock began.
const META_NAME: &str = "meta.txt";
/// The `tool=` line of `META_NAME`.
const TOOL: &str = concat!("xtask probe capture ", env!("CARGO_PKG_VERSION"));
/// The `script=` line of a run that followed a step file.
const STEPS_SCRIPT: &str = "steps";
/// The copy of the step file a run keeps, which its step marks cite by line.
const STEPS_NAME: &str = "steps.txt";
/// Nanoseconds in the seconds a clock reading is split into.
const NS_PER_SECOND: u64 = 1_000_000_000;
/// What would otherwise kill a capture with its uplink running: the two keys, a polite kill, and a terminal or session that went away.
const WATCHED_SIGNALS: [c_int; 4] = [SIGINT, SIGQUIT, SIGTERM, SIGHUP];

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
    /// Sends the stop of one route to the pods, for a capture that was killed before it could.
    Stop(StopArgs),
    /// Reads a trace written by btmon -w, or a .pklg file of Apple's packet logger, and writes its channel logs and tables into a new run, with the marks.tsv and meta.txt that analyse reads.
    Hci {
        /// The btsnoop or .pklg file: the caller's own, with no access for group or others.
        file: PathBuf,
        /// The session stamp, from date -u +%Y%m%dT%H%M%SZ.
        #[arg(long)]
        stamp: String,
        /// The name of the run to create: 1 to 32 characters of a-z, 0-9 and -.
        #[arg(long)]
        run: String,
    },
    /// Reads the marks of each trial's connection with the pods from a trace, the daemon's journal and a log of the sinks. Each row names who began the connection and gives its marks in milliseconds after the ACL link came up. The ended column holds the reason in hex, reasonless for a disconnect with no reason byte, or incomplete when a cut trace leaves the end unknown. It stays empty for an open link. A cut trace prints every trial, marks missing connections beyond its coverage incomplete, and exits with failure.
    Marks(marks::MarksArgs),
    /// Measures the shape of the audio streams in one run and writes analysis.tsv and units.tsv into it, and analysis-att.tsv, frames.tsv and att-handles.tsv when the run holds att.log.
    Analyse {
        /// The run directory, a session directory of the store then the run name.
        run_dir: PathBuf,
    },
    /// Decodes the units of an analysed run with `rusty_aac` and with FFmpeg, compares the two, and writes the decodes and decode.tsv into it.
    Decode {
        /// The run directory, a session directory of the store then the run name.
        run_dir: PathBuf,
    },
    /// Decodes the dictation frames of an analysed run with `opuscule` and with FFmpeg's two Opus decoders, compares them, and writes the decodes and decode-opus.tsv into it.
    DecodeOpus {
        /// The run directory, a session directory of the store then the run name.
        run_dir: PathBuf,
    },
    /// Measures the decoded audio of a run, alone or against a reference recording, and writes audio.tsv into it; with --whole, one decoded file as one window.
    Audio(audio::AudioArgs),
    /// Serves the audio of a capture as a microphone: its packets go through the decode chain into the node `omapods_input.probe`, each at the time the log holds for it, until the capture ends or a signal comes; one status line a second on stdout.
    Source(source::SourceArgs),
    /// The objective audio bench: makes its cache of pinned tools, or scores with them.
    Bench {
        #[command(subcommand)]
        command: bench::BenchCommand,
    },
}

#[derive(Debug, Args)]
struct StopArgs {
    /// The stream to stop: uplink or dictation.
    #[arg(long, default_value_t = Route::Uplink)]
    route: Route,
}

#[derive(Debug, Args)]
struct CaptureArgs {
    /// The stamp the session was created with.
    #[arg(long)]
    stamp: String,
    /// A new name for this run: 1 to 32 characters of a-z, 0-9 and -.
    #[arg(long)]
    run: String,
    /// The microphone stream to ask for: uplink, dictation or both.
    #[arg(long, default_value_t = Route::Uplink)]
    route: Route,
    /// The script to follow; a name that is not one is answered with the list.
    #[arg(long, required_unless_present = "steps")]
    script: Option<ScriptName>,
    /// A step file to follow in place of a script, on the dictation route: one write-request, write-command, read, control, wait or cue a line; a cue is shown and waits for a line on standard input; end it with a wait to keep the last answers.
    #[arg(long, conflicts_with_all = ["script", "minutes", "no_marker"])]
    steps: Option<PathBuf>,
    /// Lets the step file send control commands, opcode 0x0009, as written; the run cannot put a setting back, so it lasts until the pods reconnect.
    #[arg(long, conflicts_with = "script")]
    allow_control: bool,
    /// How many minutes listen and soak run, 1 to 90; no other script takes it.
    #[arg(long)]
    minutes: Option<u32>,
    /// Leaves out the two session markers around the dictation stream; refused on the uplink route.
    #[arg(long)]
    no_marker: bool,
    /// Sends the dictation start write although the pods listed no attribute; refused on the uplink route.
    #[arg(long)]
    unchecked: bool,
}

/// What a command takes from the machine, so a test can stand in for each part.
struct Host<'a> {
    home: &'a str,
    /// Opens the channel the PSM names on the device.
    connect: &'a dyn Fn(BdAddr, u16) -> Result<Channel, ChannelError>,
    /// Takes the hardware run's lock before a probe can dial the pods.
    lock_service: &'a dyn Fn() -> Result<RunLock, ServiceError>,
    stdout: &'a mut dyn Write,
    stderr: &'a mut dyn Write,
}

impl Host<'_> {
    /// One line for the owner.
    fn say(&mut self, message: &dyn fmt::Display) {
        // A standard error that cannot be written leaves nowhere to say so.
        let _unreported = writeln!(self.stderr, "xtask probe: {message}");
    }

    /// Runs `work`, which dials the pods, with the installed service stopped, and says when the service stops and when it starts again. The service is put back on every way out of `work`; a restore that failed is the error given, or is said here when `work` failed too.
    fn with_service_stopped<T, E: From<ServiceError>>(
        &mut self,
        lock: RunLock,
        work: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        let service = lock.stop()?;
        let was_running = service.was_running();
        if was_running {
            self.say(&format_args!("stopped {INSTALLED} for this run"));
        }
        let worked = work(self);
        let restored = service.restore();
        match (&worked, &restored) {
            (Err(_), Err(error)) => self.say(error),
            (_, Ok(())) if was_running => self.say(&format_args!("started {INSTALLED} again")),
            _ => {}
        }
        worked.and_then(|value| restored.map(|()| value).map_err(E::from))
    }
}

/// Why a capture or a stop did not get as far as the script; none of them carries the device address.
#[derive(Debug, thiserror::Error)]
enum ProbeError {
    #[error("HOME is not set, and the store lives under it")]
    NoHome,
    #[error("HOME {0:?} is not valid UTF-8, so no path under it could be printed as it is on disk")]
    HomeNotText(OsString),
    #[error(transparent)]
    Script(#[from] ScriptError),
    #[error("{}: {error}", .path.display())]
    Steps { path: PathBuf, error: StepsError },
    #[error("pass --script or --steps")]
    NoScript,
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error("could not watch for the signals that end a capture: {0}")]
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
    let home = match checked_home(env::var_os("HOME")) {
        Ok(home) => home,
        Err(error) => {
            report(&format!("xtask probe: {error}"));
            return ExitCode::FAILURE;
        }
    };
    let connect = |peer, psm| {
        let timeout = Duration::from_millis(CONNECT_TIMEOUT_MS);
        l2cap::connect(peer, psm, timeout)
    };
    let mut host = Host {
        home: &home,
        connect: &connect,
        lock_service: &RunLock::take,
        stdout: &mut io::stdout(),
        stderr: &mut io::stderr(),
    };
    match command {
        ProbeCommand::Init { stamp } => init(&stamp, &home),
        ProbeCommand::Capture(args) => watched(&mut host, |host, interrupted| {
            capture(&args, host, interrupted)
        }),
        ProbeCommand::Stop(args) => watched(&mut host, |host, interrupted| {
            stop(host, args.route, interrupted)
        }),
        ProbeCommand::Hci { file, stamp, run } => hci(&file, &stamp, &run, &home),
        ProbeCommand::Marks(args) => marks(&args),
        ProbeCommand::Analyse { run_dir } => print_table(analyse::analyse_run(&run_dir)),
        ProbeCommand::Decode { run_dir } => print_table(decode::decode_run(&run_dir)),
        ProbeCommand::DecodeOpus { run_dir } => print_table(decode_opus::decode_run(&run_dir)),
        ProbeCommand::Audio(args) => print_table(audio::audio_run(&args)),
        ProbeCommand::Source(args) => watched(&mut host, |host, interrupted| {
            source::serve(&args, host, interrupted)
        }),
        ProbeCommand::Bench { command } => bench::run(&command),
    }
}

/// Runs a command that a signal ends early, with the flag the signal raises.
fn watched(
    host: &mut Host<'_>,
    command: impl FnOnce(&mut Host<'_>, &AtomicBool) -> ExitCode,
) -> ExitCode {
    match watch_signals() {
        Ok(interrupted) => command(host, &interrupted),
        Err(error) => {
            host.say(&error);
            ExitCode::FAILURE
        }
    }
}

/// A lossy print of a home would name a directory that does not exist, so only text passes.
fn checked_home(home: Option<OsString>) -> Result<String, ProbeError> {
    home.ok_or(ProbeError::NoHome)?
        .into_string()
        .map_err(ProbeError::HomeNotText)
}

/// Opens the session for `stamp` and prints its path, which is all init writes on stdout.
fn init(stamp: &str, home: &str) -> ExitCode {
    match Session::open(&store::root(home.as_ref()), stamp) {
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

/// Prints the table a command wrote into a run, which is all it writes on stdout, or the reason it wrote none.
fn print_table(written: Result<String, impl fmt::Display>) -> ExitCode {
    match written {
        Ok(table) => {
            // Only the last newline goes, for print_line to add back: the last row may end in a tab.
            print_line(table.strip_suffix('\n').unwrap_or(&table));
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
        Ok(outcome) => exit_code(&outcome),
        Err(error) => {
            host.say(&error);
            ExitCode::FAILURE
        }
    }
}

/// Makes the run and its files, then, with the installed service stopped, dials the pods, follows the script and says the summary; a service that could not be stopped ends the run's marks with the reason.
fn record(
    args: &CaptureArgs,
    host: &mut Host<'_>,
    interrupted: &AtomicBool,
) -> Result<Outcome, ProbeError> {
    let (script, steps_text) = plan(args)?;
    let script = if args.unchecked {
        script.allow_unchecked(args.route)?
    } else {
        script
    };
    let root = store::root(host.home.as_ref());
    let session = Session::open(&root, &args.stamp)?;
    // The device is read before the run is created, so a missing device file does not use up a run name.
    let peer = store::read_device(&root)?;
    // The lock is taken before the run is created, so a capture refused for a held lock does not use up a run name.
    let lock = (host.lock_service)()?;
    let run = Run::create(&session, &args.run)?;
    let mut log = run.create_file(LOG_NAME)?;
    let mut att_log = match args.route {
        Route::Uplink => None,
        Route::Dictation | Route::Both => Some(run.create_file(ATT_LOG_NAME)?),
    };
    let mut handles = if script.walks() {
        Some(run.create_file(HANDLES_NAME)?)
    } else {
        None
    };
    let mut marks = run.create_file(MARKS_NAME)?;
    // The copy is kept before meta.txt names it.
    if let Some(text) = &steps_text {
        let written = run.create_file(STEPS_NAME)?.write_all(text);
        written.map_err(|source| ProbeError::Write {
            file: STEPS_NAME,
            source,
        })?;
    }
    let mut meta = run.create_file(META_NAME)?;
    let zero_ns = clock_ns(ClockId::Monotonic);
    let realtime_at_zero_ns = clock_ns(ClockId::Realtime);
    let now_ns = || clock_ns(ClockId::Monotonic).saturating_sub(zero_ns);
    let script_name: &dyn fmt::Display = match &args.script {
        Some(name) => name,
        None => &STEPS_SCRIPT,
    };
    // A run of the uplink route keeps the three lines it always had.
    let route_line = match args.route {
        Route::Uplink => String::new(),
        Route::Dictation | Route::Both => {
            // A step file holds its own markers, so its run names the copy it keeps instead.
            let marker = match &steps_text {
                Some(_) => format!("steps={STEPS_NAME}"),
                None => format!("no_marker={}", args.no_marker),
            };
            format!(
                "route={}\n{marker}\nunchecked={}\n",
                args.route, args.unchecked
            )
        }
    };
    let about = meta_text(script_name, TOOL, realtime_at_zero_ns) + &route_line;
    let written = meta.write_all(about.as_bytes());
    written.map_err(|source| ProbeError::Write {
        file: META_NAME,
        source,
    })?;
    let connect = host.connect;
    let mut reached_dial = false;
    // The files above are made first, so a capture refused for its arguments or its store never stops the service.
    let recorded = host.with_service_stopped(lock, |host| {
        reached_dial = true;
        let accessory = match dial_control(connect, peer, script.opening) {
            Ok(accessory) => accessory,
            Err(error) => {
                // The connect error is what the owner is told; the mark only keeps it with the run.
                let _unmarked = capture::write_mark(&mut marks, now_ns(), Mark::End, &error);
                return Err(ProbeError::Connect(error));
            }
        };
        let open_att = || connect(peer, ATT_PSM);
        let links = Links {
            accessory: accessory.as_ref(),
            att: att_of(args.route, &open_att),
        };
        let att_log: Option<&mut dyn Write> = match &mut att_log {
            Some(file) => Some(file),
            None => None,
        };
        let handles: Option<&mut dyn Write> = match &mut handles {
            Some(file) => Some(file),
            None => None,
        };
        let mut out = Outputs {
            log: &mut log,
            att_log,
            handles,
            marks: &mut marks,
            cues: &mut *host.stdout,
        };
        let outcome = capture::run(&links, &script, &mut out, interrupted, &now_ns);
        host.say(&summary(&outcome, args.route));
        Ok(outcome)
    });
    if !reached_dial && let Err(error) = &recorded {
        // The run is made already, so what kept it from its dial ends its marks, as a refused connect does.
        let _unmarked = capture::write_mark(&mut marks, now_ns(), Mark::End, error);
    }
    recorded
}

/// The named script, or the one a step file plans with the file's text, read whole before anything is opened.
fn plan(args: &CaptureArgs) -> Result<(Script, Option<Vec<u8>>), ProbeError> {
    match (&args.steps, args.script) {
        (Some(path), _) => {
            let text = store::read_trace(path, script::MAX_STEPS_FILE_BYTES)?;
            let events = script::parse_steps(&text, args.allow_control).map_err(|error| {
                ProbeError::Steps {
                    path: path.clone(),
                    error,
                }
            })?;
            Ok((script::stepped(args.route, events)?, Some(text)))
        }
        (None, Some(name)) => {
            let script = script::build(name, args.route, args.minutes, !args.no_marker)?;
            Ok((script, None))
        }
        // The parser takes one of the two before this runs.
        (None, None) => Err(ProbeError::NoScript),
    }
}

/// Opens the control channel, unless the script opens the attribute channel alone.
fn dial_control(
    connect: &dyn Fn(BdAddr, u16) -> Result<Channel, ChannelError>,
    peer: BdAddr,
    opening: Opening,
) -> Result<Option<Channel>, ChannelError> {
    match opening {
        Opening::AttKnock | Opening::AttMap => Ok(None),
        Opening::Handshake | Opening::Knock | Opening::EarlyStart => {
            connect(peer, CONTROL_PSM).map(Some)
        }
    }
}

/// The way to the attribute channel on every route but uplink, which the run opens itself.
fn att_of(route: Route, open: OpenAtt<'_>) -> Option<OpenAtt<'_>> {
    match route {
        Route::Uplink => None,
        Route::Dictation | Route::Both => Some(open),
    }
}

/// Connects, runs the handshake and sends the stop of `route`, without a run directory and without a start; a raised `interrupted` ends it early.
fn stop(host: &mut Host<'_>, route: Route, interrupted: &AtomicBool) -> ExitCode {
    match rescue(host, route, interrupted) {
        Ok(outcome) => exit_code(&outcome),
        Err(error) => {
            host.say(&error);
            ExitCode::FAILURE
        }
    }
}

/// The one sentence a stop ends with.
fn rescue_summary(outcome: &Outcome, route: Route) -> String {
    let (end, done) = (&outcome.end, outcome.end == EndReason::ScriptDone);
    match route {
        Route::Uplink if done => {
            format!("STOP was sent and the channel was kept open for {STOP_TAIL_MS} ms")
        }
        Route::Uplink => format!("STOP was not sent: {end}"),
        // A stop of both routes is refused before anything is dialed, so the arms below name it for the match alone.
        Route::Dictation | Route::Both if done => format!(
            "the dictation stop and the idle session marker were sent and the channels were kept open for {STOP_TAIL_MS} ms"
        ),
        Route::Dictation | Route::Both => {
            let stops = outcome.dictation_stops;
            format!("the stop ended early: {end} (dictation stop sent {stops})")
        }
    }
}

/// With the installed service stopped, dials the pods, sends the stop and says the summary.
fn rescue(
    host: &mut Host<'_>,
    route: Route,
    interrupted: &AtomicBool,
) -> Result<Outcome, ProbeError> {
    let script = script::rescue(route)?;
    let root = store::root(host.home.as_ref());
    let peer = store::read_device(&root)?;
    let connect = host.connect;
    let lock = (host.lock_service)()?;
    host.with_service_stopped(lock, |host| {
        let accessory = dial_control(connect, peer, script.opening).map_err(ProbeError::Connect)?;
        let open_att = || connect(peer, ATT_PSM);
        // Nothing of a rescue is kept: a signal that ends it early has only cost a STOP that can be sent again.
        let (mut log, mut att_log, mut marks, mut cues) =
            (io::sink(), io::sink(), io::sink(), io::sink());
        let mut out = Outputs {
            log: &mut log,
            att_log: Some(&mut att_log),
            handles: None,
            marks: &mut marks,
            cues: &mut cues,
        };
        let now_ns = || clock_ns(ClockId::Monotonic);
        let links = Links {
            accessory: accessory.as_ref(),
            att: att_of(route, &open_att),
        };
        let outcome = capture::run(&links, &script, &mut out, interrupted, &now_ns);
        host.say(&rescue_summary(&outcome, route));
        Ok(outcome)
    })
}

fn exit_code(outcome: &Outcome) -> ExitCode {
    if outcome.end == EndReason::ScriptDone {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The one sentence a capture ends with: the reason, whether each stop followed its start, and the counts, to which every route but uplink adds those of the attribute channel.
fn summary(outcome: &Outcome, route: Route) -> String {
    let Outcome {
        sdus_in,
        audio_sdus,
        starts,
        stops,
        uplink_running,
        truncated,
        att_pdus_in,
        audio_notifications,
        dictation_starts,
        dictation_stops,
        att_requests_refused,
        end,
    } = outcome;
    let att_counts = match route {
        Route::Uplink => String::new(),
        Route::Dictation | Route::Both => format!(
            ", attribute packets in {att_pdus_in}, with audio {audio_notifications}, dictation start sent {dictation_starts}, dictation stop sent {dictation_stops}, requests refused {att_requests_refused}"
        ),
    };
    let counts = format!(
        "packets in {sdus_in}, with audio {audio_sdus}, not logged {truncated}, START sent {starts}, STOP sent {stops}{att_counts}"
    );
    if *end == EndReason::ScriptDone {
        return format!("capture complete: {counts}");
    }
    let stop = if *starts == 0 {
        ""
    } else if !*uplink_running {
        "; STOP was sent"
    } else {
        "; STOP was NOT sent, so run cargo xtask probe stop"
    };
    let dictation_stop = if *dictation_starts == 0 {
        ""
    } else if dictation_stops >= dictation_starts {
        "; the dictation stop was sent"
    } else {
        "; the dictation stop was NOT sent, so run cargo xtask probe stop --route dictation"
    };
    format!("capture ended early: {end}{stop}{dictation_stop} ({counts})")
}

/// Raises the returned flag on each of `WATCHED_SIGNALS`, which the loop then sees within one receive timeout.
fn watch_signals() -> Result<Arc<AtomicBool>, ProbeError> {
    let interrupted = Arc::new(AtomicBool::new(false));
    for signal in WATCHED_SIGNALS {
        signal_hook::flag::register(signal, Arc::clone(&interrupted))
            .map_err(ProbeError::Signals)?;
    }
    Ok(interrupted)
}

/// A clock reading in nanoseconds, as `tv_sec 1763730000` and `tv_nsec 5` read 1763730000000000005; a reading before the clock's own zero cannot happen and reads as that zero.
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

/// Writes the tables of one trace into a new run and prints the counts, which is all hci writes on stdout; a cut trace is named on stderr.
fn hci(file: &Path, stamp: &str, run: &str, home: &str) -> ExitCode {
    match hci::export(&store::root(home.as_ref()), file, stamp, run) {
        Ok(exported) => {
            if let Some(cut) = exported.cut {
                report(&format!("xtask probe: {}: {cut}", file.display()));
            }
            exported.summary.lines().for_each(print_line);
            ExitCode::SUCCESS
        }
        Err(error) => {
            report(&format!("xtask probe: {error}"));
            ExitCode::FAILURE
        }
    }
}

/// Prints the table of one sitting and fails if the trace was cut, with the reason on stderr.
fn marks(args: &marks::MarksArgs) -> ExitCode {
    let marked = marks::marks(args);
    let cut = marked.as_ref().ok().and_then(|(_, cut)| *cut);
    let printed = print_table(marked.map(|(table, _)| table));
    if let Some(cut) = cut {
        report(&format!("xtask probe: {cut}"));
        return ExitCode::FAILURE;
    }
    printed
}
