//! `probe source`: serves the pods' uplink, or a capture of it, as a microphone. Its audio packets go through the decode chain of `pods-mic` into one stream source node; a capture's each at the time the log holds for it. The unit tests are support for the tests of the built command.

mod live;

use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use clap::Args;
use pods_mic::{Chain, Counters, NODE_LATENCY_MS, RATE_HZ, Reader, Resync};
use pods_proto::sdulog::Direction;
use pods_proto::uplink::UplinkPacket;
use pods_pw::{Event, Fill, OpenError, Source, Spec};
use pods_sys::l2cap::ChannelError;

use super::Host;
use super::analyse::MAX_LOG_BYTES;
use super::analyse::input::{InputError, LOG_FILE, parse_log, text_of, whole_lines};
use super::store::{self, StoreError};

const NODE_NAME: &str = "omapods_input.probe";
const NODE_DESCRIPTION: &str = "AirPods Microphone (probe)";
/// The lowest session priority, so no session manager prefers the probe's node over a device.
const NODE_PRIORITY: u32 = 0;
/// The name of the session's audio server socket inside the runtime directory.
const SERVER_SOCKET: &str = "pipewire-0";
/// Bounds how late a signal is seen; chosen here.
const POLL: Duration = Duration::from_millis(10);
const STATUS_EVERY: Duration = Duration::from_secs(1);
/// Reads in a row past the capture's last frame before the node goes; on a test server two still cut the recording's end and three do not.
const DRAIN_READS: u32 = 3;
/// A graph that runs no cycle for a second has stalled, so the node goes without its tail.
const TAIL_BOUND: Duration = Duration::from_secs(1);
const NS_PER_MS: u64 = 1_000_000;
const PARTS_PER_MILLION: f64 = 1e6;
/// What the status line shows for a figure no read has given yet.
const NONE: &str = "none";

#[derive(Debug, Args)]
pub(super) struct SourceArgs {
    /// The sdu.log of a run to serve: the caller's own, with no access for group or others; without it the pods of the device file are served live.
    #[arg(long)]
    replay: Option<PathBuf>,
    /// How many seconds a live run serves before it stops the uplink; without it, until a signal.
    #[arg(long, conflicts_with = "replay")]
    seconds: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
enum SourceError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{}: {error}", .path.display())]
    Input { path: PathBuf, error: InputError },
    #[error("{} holds no audio packet from the pods", .0.display())]
    NoAudio(PathBuf),
    #[error("the decoder refused the configuration of the stream")]
    Decoder,
    #[error("XDG_RUNTIME_DIR is not set, and the audio server's socket lives in it")]
    NoRuntimeDir,
    #[error(transparent)]
    Open(#[from] OpenError),
    #[error("the audio server went away")]
    Lost,
    #[error("the recorder had not read the capture's end {} ms after it was due, so its last audio may be missing", TAIL_BOUND.as_millis())]
    TailUnread,
    #[error("could not print the status line: {0}")]
    Print(io::Error),
    #[error("{0}")]
    Service(String),
    #[error("a hardware run or another live probe is in progress, so nothing was stopped")]
    LockHeld,
    #[error("{0}")]
    Connect(ChannelError),
    #[error("the control channel failed: {0}")]
    Channel(ChannelError),
    #[error("the pods closed the control channel")]
    PeerClosed,
    #[error("the pods did not answer the handshake within {} ms", live::HANDSHAKE_STEP.as_millis())]
    Handshake,
    #[error("the pods did not answer START within {} ms, so nothing more was sent", live::START_ANSWER.as_millis())]
    StartUnanswered,
    #[error("the pods did not answer STOP within {} ms", live::STOP_ANSWER.as_millis())]
    StopUnanswered,
    #[error("the pods stopped the uplink on their own")]
    PodsStopped,
    #[error("the restore of the device's sink failed: {0}")]
    Restore(String),
}

/// One audio packet of a capture: when it landed on the log's clock, and its units as stamp and body.
struct Packet {
    at_ns: u64,
    units: Vec<(u32, Vec<u8>)>,
}

/// The audio packets of a capture, and how many packets from the pods were cut short before their fixed fields.
struct Audio {
    packets: Vec<Packet>,
    cut_short: usize,
}

/// What the real time half last saw, as the bits of two floats, whether it has read at all, how often, and how many reads in a row ended past the last frame written.
#[derive(Debug, Default)]
struct Seen {
    read: AtomicBool,
    ahead_ms: AtomicU64,
    rate_ppm: AtomicU64,
    reads: AtomicU64,
    past_end: AtomicU32,
}

/// The real time half: the reader, the instant it and the feeding half count from, and its reads in a row past the end.
struct Held {
    reader: Reader,
    since: Instant,
    seen: Arc<Seen>,
    past_end: u32,
}

impl Fill for Held {
    fn fill(&mut self, out: &mut [f32]) -> f64 {
        let ratio = self.reader.read(self.since.elapsed(), out);
        self.past_end = if self.reader.read_all() {
            self.past_end.saturating_add(1)
        } else {
            0
        };
        let ahead_ms = self.reader.ahead_ms().to_bits();
        self.seen.ahead_ms.store(ahead_ms, Ordering::Relaxed);
        let rate_ppm = ((ratio - 1.0) * PARTS_PER_MILLION).to_bits();
        self.seen.rate_ppm.store(rate_ppm, Ordering::Relaxed);
        self.seen.past_end.store(self.past_end, Ordering::Relaxed);
        // Release after the figures, so a status line or a tail that sees the flag or the count sees them too.
        self.seen.read.store(true, Ordering::Release);
        self.seen.reads.fetch_add(1, Ordering::Release);
        ratio
    }
}

/// The node and what its status line reads.
struct Served {
    source: Source,
    seen: Arc<Seen>,
    counters: Arc<Counters>,
    /// Gets a word when the server is lost.
    gone: Receiver<()>,
    since: Instant,
}

impl Served {
    fn print_status(&self, out: &mut dyn Write) -> Result<(), SourceError> {
        let line = status_line(&self.seen, self.source.unsteered(), &self.counters);
        writeln!(out, "{line}").map_err(SourceError::Print)
    }
}

/// The status line: what the reader last saw, none before a recorder first reads, the periods the node could not steer, and the chain's counters.
fn status_line(seen: &Seen, unsteered: u64, counters: &Counters) -> String {
    let float = |bits: &AtomicU64| f64::from_bits(bits.load(Ordering::Relaxed));
    let count = |counter: &AtomicU32| counter.load(Ordering::Relaxed);
    let (distance, rate) = if seen.read.load(Ordering::Acquire) {
        let distance = format!("{:.3} ms", float(&seen.ahead_ms));
        (distance, format!("{:.1} ppm", float(&seen.rate_ppm)))
    } else {
        (NONE.to_owned(), NONE.to_owned())
    };
    format!(
        "distance {distance}, rate {rate}, unsteered {unsteered}, underruns {}, concealed {}, decode errors {}, decoder resets {}",
        count(&counters.underruns),
        count(&counters.concealed),
        count(&counters.decode_errors),
        count(&counters.decoder_resets),
    )
}

/// Serves a capture to its end or the pods' uplink for its seconds, either until a signal, with a status line a second and one at the end; any error the run says fails it.
pub(super) fn serve(args: &SourceArgs, host: &mut Host<'_>, interrupted: &AtomicBool) -> ExitCode {
    let served = match &args.replay {
        Some(file) => replay(file, host, interrupted),
        None => live::serve(args.seconds, host, interrupted),
    };
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            host.say(&error);
            ExitCode::FAILURE
        }
    }
}

fn replay(file: &Path, host: &mut Host<'_>, interrupted: &AtomicBool) -> Result<(), SourceError> {
    // Read whole before the node exists, so a capture that cannot be served never shows one.
    let audio = audio_of(file)?;
    if audio.cut_short > 0 {
        let count = audio.cut_short;
        let path = file.display();
        host.say(&format_args!(
            "{path}: packets from the pods cut short before their fixed fields, left out: {count}"
        ));
    }
    if audio.packets.is_empty() {
        return Err(SourceError::NoAudio(file.to_owned()));
    }
    let counters = Arc::new(Counters::default());
    let (mut chain, reader, resync) =
        pods_mic::chain(Arc::clone(&counters)).ok_or(SourceError::Decoder)?;
    let served = open_node(reader, resync, counters)?;
    feed(
        &audio.packets,
        &mut chain,
        &served,
        host.stdout,
        interrupted,
    )?;
    served.print_status(host.stdout)
}

/// Opens the node on the session's audio server, its reader counting from now.
fn open_node(
    reader: Reader,
    resync: Resync,
    counters: Arc<Counters>,
) -> Result<Served, SourceError> {
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR").ok_or(SourceError::NoRuntimeDir)?;
    let socket = Path::new(&runtime_dir).join(SERVER_SOCKET);
    let seen = Arc::new(Seen::default());
    let (lost, gone) = mpsc::channel();
    // Only the origin of the clock: every wait below counts from a reading taken after the node is open.
    let since = Instant::now();
    let held = Held {
        reader,
        since,
        seen: Arc::clone(&seen),
        past_end: 0,
    };
    let spec = Spec {
        remote: &socket,
        name: NODE_NAME,
        description: NODE_DESCRIPTION,
        rate: RATE_HZ,
        latency_ns: u64::from(NODE_LATENCY_MS).saturating_mul(NS_PER_MS),
        priority: NODE_PRIORITY,
    };
    Ok(Served {
        source: Source::open(&spec, Box::new(held), Box::new(on_event(resync, lost)))?,
        seen,
        counters,
        gone,
        since,
    })
}

/// What the node's events do: a recorder that links starts at the reader's target, and a lost server ends the replay.
fn on_event(resync: Resync, lost: Sender<()>) -> impl FnMut(Event) + Send {
    move |event| match event {
        Event::Streaming => resync.request(),
        Event::Paused => {}
        // The receiver is gone only once the replay has ended.
        Event::Lost => drop(lost.send(())),
    }
}

/// The audio packets the pods sent, in the order of the log, and a count of those the uplink parser refused.
fn audio_of(file: &Path) -> Result<Audio, SourceError> {
    let refused = |error| SourceError::Input {
        path: file.to_owned(),
        error,
    };
    let bytes = store::read_trace(file, MAX_LOG_BYTES)?;
    // A last line without its newline is one a killed capture was still writing, and is left out.
    let (whole, _) = whole_lines(&bytes);
    let records = parse_log(text_of(LOG_FILE, whole).map_err(refused)?).map_err(refused)?;
    let from_pods = records
        .iter()
        .filter(|record| record.direction == Direction::FromPods);
    let mut audio = Audio {
        packets: Vec::new(),
        cut_short: 0,
    };
    for record in from_pods {
        match UplinkPacket::parse(&record.sdu) {
            Ok(Some(UplinkPacket::Audio(packet))) => audio.packets.push(Packet {
                at_ns: record.at_ns,
                units: packet
                    .units()
                    .map(|unit| (unit.stamp, unit.data.to_vec()))
                    .collect(),
            }),
            Ok(Some(UplinkPacket::Control(_) | UplinkPacket::Unknown { .. }) | None) => {}
            // The uplink parser refuses only a packet cut before its fixed fields.
            Err(_) => audio.cut_short = audio.cut_short.saturating_add(1),
        }
    }
    Ok(audio)
}

/// Hands each packet to the chain when the log says it landed, counted from now on the monotonic clock, then keeps the node while the reader plays what it holds; a signal ends it early.
fn feed(
    packets: &[Packet],
    chain: &mut Chain,
    served: &Served,
    out: &mut dyn Write,
    interrupted: &AtomicBool,
) -> Result<(), SourceError> {
    let began = served.since.elapsed();
    let first_ns = packets.first().map_or(0, |packet| packet.at_ns);
    let mut clock = Clock {
        status_due: began.saturating_add(STATUS_EVERY),
        out,
        interrupted,
    };
    let mut handed = began;
    for packet in packets {
        let after_first = Duration::from_nanos(packet.at_ns.saturating_sub(first_ns));
        let due = began.saturating_add(after_first);
        if clock.wait_until(due, &|| false, served)? == Waited::Signal {
            return Ok(());
        }
        handed = served.since.elapsed();
        let units = packet.units.iter();
        chain.sdu(handed, units.map(|(stamp, body)| (*stamp, body.as_slice())));
    }
    let seen = &served.seen;
    // The reader holds the node's stated latency of audio behind the last packet it was given.
    let played = handed.saturating_add(Duration::from_millis(u64::from(NODE_LATENCY_MS)));
    if clock.wait_until(played, &|| false, served)? == Waited::Signal
        || !seen.read.load(Ordering::Acquire)
    {
        return Ok(());
    }
    // Counted from a latency after the last write, so no read begun before that write is among them.
    let reads_at_end = seen.reads.load(Ordering::Acquire);
    let carried = || {
        let reads = seen
            .reads
            .load(Ordering::Acquire)
            .saturating_sub(reads_at_end);
        reads >= u64::from(DRAIN_READS) && seen.past_end.load(Ordering::Relaxed) >= DRAIN_READS
    };
    // A recorder that reads and stops short of the end, or a stalled graph, leaves the recording without its end.
    match clock.wait_until(played.saturating_add(TAIL_BOUND), &carried, served)? {
        Waited::Done | Waited::Signal => Ok(()),
        Waited::Due => Err(SourceError::TailUnread),
    }
}

/// Why a wait of the replay ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waited {
    Due,
    /// What it waited for came first.
    Done,
    Signal,
}

/// The waits of a replay: the next status line, where it goes, and the signal that ends the replay.
struct Clock<'a> {
    status_due: Duration,
    out: &'a mut dyn Write,
    interrupted: &'a AtomicBool,
}

impl Clock<'_> {
    /// Waits until `due` on the replay's clock or until `done` holds, printing each status line as it falls due.
    fn wait_until(
        &mut self,
        due: Duration,
        done: &dyn Fn() -> bool,
        served: &Served,
    ) -> Result<Waited, SourceError> {
        loop {
            if self.interrupted.load(Ordering::SeqCst) {
                return Ok(Waited::Signal);
            }
            if done() {
                return Ok(Waited::Done);
            }
            let now = served.since.elapsed();
            if now >= self.status_due {
                served.print_status(self.out)?;
                self.status_due = self.status_due.saturating_add(STATUS_EVERY);
            }
            // A wait of zero still takes a word already sent, so an overdue packet sees a lost server too.
            let wait = due.min(self.status_due).saturating_sub(now).min(POLL);
            match served.gone.recv_timeout(wait) {
                Err(RecvTimeoutError::Timeout) => {}
                // The sender lives in the node's callbacks, which end only with the node.
                Ok(()) | Err(RecvTimeoutError::Disconnected) => return Err(SourceError::Lost),
            }
            if now >= due {
                return Ok(Waited::Due);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pods_mic::{SAMPLES_PER_UNIT, STAMP_STEP};
    use pods_samples::uplink::tone_unit;

    use super::*;

    /// The units of one packet, and the packets fed before the first read.
    const UNITS: u32 = 4;
    const PACKETS: u32 = 8;
    /// A tone the decoder takes: its line, and its level.
    const LINE: u16 = 15;
    const LEVEL: u8 = 186;

    #[test]
    fn the_status_line_names_each_figure_it_holds() {
        let seen = Seen::default();
        let counters = Counters::default();
        assert_eq!(
            status_line(&seen, 0, &counters),
            "distance none, rate none, unsteered 0, underruns 0, concealed 0, decode errors 0, decoder resets 0"
        );
        seen.read.store(true, Ordering::Release);
        seen.ahead_ms.store(1.5_f64.to_bits(), Ordering::Relaxed);
        seen.rate_ppm
            .store((-82.44_f64).to_bits(), Ordering::Relaxed);
        counters.underruns.store(4, Ordering::Relaxed);
        counters.concealed.store(5, Ordering::Relaxed);
        counters.decode_errors.store(6, Ordering::Relaxed);
        counters.decoder_resets.store(7, Ordering::Relaxed);
        assert_eq!(
            status_line(&seen, 3, &counters),
            "distance 1.500 ms, rate -82.4 ppm, unsteered 3, underruns 4, concealed 5, decode errors 6, decoder resets 7"
        );
    }

    /// A reader with packets behind it, and the chain that keeps them.
    fn fed() -> (Chain, Held, Resync) {
        let (mut chain, reader, resync) = pods_mic::chain(Arc::default()).unwrap();
        let held = Held {
            reader,
            since: Instant::now(),
            seen: Arc::default(),
            past_end: 0,
        };
        let unit = tone_unit(LINE, LEVEL);
        for packet in 0..PACKETS {
            let first = packet.saturating_mul(UNITS);
            let stamps =
                (first..first.saturating_add(UNITS)).map(|unit| unit.saturating_mul(STAMP_STEP));
            let units = stamps.map(|stamp| (stamp, unit.as_slice()));
            chain.sdu(held.since.elapsed(), units);
        }
        (chain, held, resync)
    }

    #[test]
    fn a_fill_leaves_the_readers_distance_and_the_rate_it_asked_for() {
        let (_chain, mut held, _resync) = fed();
        let mut out = vec![0.0; usize::from(SAMPLES_PER_UNIT)];
        let mut moved = 0;
        // Reads one after the other, with no time between them, put the reader ahead of its target.
        for _ in 0..PACKETS {
            let ratio = held.fill(&mut out);
            let ahead_ms = held.seen.ahead_ms.load(Ordering::Relaxed);
            let rate_ppm = held.seen.rate_ppm.load(Ordering::Relaxed);
            assert_eq!(ahead_ms, held.reader.ahead_ms().to_bits());
            assert_eq!(rate_ppm, ((ratio - 1.0) * PARTS_PER_MILLION).to_bits());
            moved += u32::from(ahead_ms != rate_ppm);
        }
        assert!(moved > 0, "no read left a distance apart from its rate");
    }

    #[test]
    fn a_recorder_that_links_puts_the_reader_on_its_target_and_a_lost_server_is_told() {
        let (_chain, mut held, resync) = fed();
        let (lost, gone) = mpsc::channel();
        let mut events = on_event(resync, lost);
        let mut out = vec![0.0; usize::from(SAMPLES_PER_UNIT)];
        // The second read, with no time since the first, leaves the reader one read ahead.
        held.fill(&mut out);
        held.fill(&mut out);
        assert_ne!(held.reader.ahead_ms().to_bits(), 0.0_f64.to_bits());
        events(Event::Streaming);
        held.fill(&mut out);
        assert_eq!(held.reader.ahead_ms().to_bits(), 0.0_f64.to_bits());
        events(Event::Paused);
        assert!(gone.try_recv().is_err());
        events(Event::Lost);
        assert_eq!(gone.try_recv(), Ok(()));
    }
}
