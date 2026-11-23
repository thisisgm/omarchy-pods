//! The live half of `probe source`: the installed service stopped, the pods' uplink from START to STOP, each audio packet handed to the chain as it lands, then the device's sink suspended and resumed.

use std::ffi::OsStr;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use pods_core::policy::audio::sink_prefix;
use pods_mic::{Chain, Counters};
use pods_proto::addr::BdAddr;
use pods_proto::handshake::{Handshake, Step};
use pods_proto::packets::{CONTROL_PSM, HANDSHAKE};
use pods_proto::uplink::{self, START, STOP, UplinkPacket, UplinkState};
use pods_sys::l2cap::{Channel, ChannelError, Received};
use rustix::io::Errno;

use super::{POLL, STATUS_EVERY, Served, SourceError, open_node};
use crate::hw::run_lock;
use crate::hw::unit::{self, Guard, INSTALLED};
use crate::probe::{HANDSHAKE_STEP_TIMEOUT_MS, Host, store};

pub(super) const HANDSHAKE_STEP: Duration = Duration::from_millis(HANDSHAKE_STEP_TIMEOUT_MS);
/// Well over the slowest answer to START seen, which came in under a second.
pub(super) const START_ANSWER: Duration = Duration::from_millis(1500);
/// Well over the slowest answer to STOP seen, which came in under a second.
pub(super) const STOP_ANSWER: Duration = Duration::from_millis(1500);
/// How long the device's sink stays suspended, the form of the restore measured on the test machine.
const PROBE_RESTORE_HOLD_MS: u64 = 300;
/// One `pactl` call on a local server ends well inside this.
const PACTL_TIMEOUT: Duration = Duration::from_secs(5);
/// Larger than any packet the pods send.
const RECV_BUFFER_LEN: usize = 4096;
/// What starts each line the uplink's guard reports from a drop.
const GUARD_SAYS: &str = "xtask probe:";

/// Stops the installed service, serves the pods' uplink until `seconds` pass or a signal comes, then puts the service back as it was.
pub(super) fn serve(
    seconds: Option<u64>,
    host: &mut Host<'_>,
    interrupted: &AtomicBool,
) -> Result<(), SourceError> {
    let peer = store::read_device(&store::root(host.home.as_ref()))?;
    // Held from before the installed service's state is read until after its restore, so a hardware run and this probe never share the service.
    let lock = run_lock()
        .map_err(SourceError::Service)?
        .ok_or(SourceError::LockHeld)?;
    let word = unit::activity(INSTALLED).map_err(SourceError::Service)?;
    let was_running = unit::is_running(&word);
    // Made before the stop, as the hardware run makes it, so a stop that fails partway and every later way out, an unwind included, start it again.
    let service = Guard::installed_only(was_running);
    let stopped = if was_running {
        unit::stop(INSTALLED).map_err(SourceError::Service)
    } else {
        Ok(())
    };
    let served = stopped.and_then(|()| uplink(peer, seconds, host, interrupted));
    let restored = service.restore().map_err(SourceError::Service);
    if let (Err(_), Err(error)) = (&served, &restored) {
        host.say(error);
    }
    drop(lock);
    served.and(restored)
}

/// Opens the control channel, runs the handshake and START, and serves the uplink once the pods say it runs.
fn uplink(
    peer: BdAddr,
    seconds: Option<u64>,
    host: &mut Host<'_>,
    interrupted: &AtomicBool,
) -> Result<(), SourceError> {
    let channel = (host.connect)(peer, CONTROL_PSM).map_err(SourceError::Connect)?;
    channel
        .set_recv_timeout(POLL)
        .map_err(SourceError::Channel)?;
    let mut buf = [0_u8; RECV_BUFFER_LEN];
    handshake(&channel, &mut buf)?;
    let counters = Arc::new(Counters::default());
    let (mut chain, reader, resync) =
        pods_mic::chain(Arc::clone(&counters)).ok_or(SourceError::Decoder)?;
    channel.send(&START).map_err(SourceError::Channel)?;
    // A signal does not end this wait: no STOP goes out while a START is unanswered.
    let running = wait_for(&channel, &mut buf, START_ANSWER, |sdu| {
        (uplink::state::parse(sdu) == Some(UplinkState::Running)).then_some(())
    })?;
    if running.is_none() {
        return Err(SourceError::StartUnanswered);
    }
    let mut running = Running {
        channel: &channel,
        peer,
        pending: true,
    };
    let served = open_node(reader, resync, counters);
    // The last status line is printed apart from the feed, so a failed print cannot hide the pods' own stop.
    let (ended, printed) = match &served {
        Ok(served) => {
            let until = seconds
                .and_then(|seconds| Instant::now().checked_add(Duration::from_secs(seconds)));
            let fed = feed(
                &channel,
                &mut buf,
                &mut chain,
                served,
                host.stdout,
                interrupted,
                until,
            );
            let printed = match fed {
                Ok(_) => served.print_status(host.stdout),
                Err(_) => Ok(()),
            };
            (fed, printed)
        }
        Err(_) => (Ok(Ended::Asked), Ok(())),
    };
    let by_pods = matches!(ended, Ok(Ended::ByPods));
    let (stopped, restored) = running.finish(&mut buf, by_pods);
    if let Ok(Restored::Done) = &restored {
        host.say(&format_args!(
            "the device's sink was suspended for {PROBE_RESTORE_HOLD_MS} ms and resumed"
        ));
    }
    let pods_stopped = by_pods.then_some(SourceError::PodsStopped);
    let errors = [
        served.err(),
        ended.err(),
        printed.err(),
        stopped.err(),
        restored.err(),
    ];
    let mut errors = errors.into_iter().chain([pods_stopped]).flatten();
    // The caller says the first; any later one is said here, ahead of it.
    let first = errors.next();
    for later in errors {
        host.say(&later);
    }
    first.map_or(Ok(()), Err)
}

/// Sends the handshake's requests, each once the pods acknowledged the one before.
fn handshake(channel: &Channel, buf: &mut [u8]) -> Result<(), SourceError> {
    let mut handshake = Handshake::default();
    let mut request: &[u8] = &HANDSHAKE;
    loop {
        channel.send(request).map_err(SourceError::Channel)?;
        if handshake.step() == Step::Ready {
            return Ok(());
        }
        request = wait_for(channel, buf, HANDSHAKE_STEP, |sdu| handshake.on_packet(sdu))?
            .ok_or(SourceError::Handshake)?;
    }
}

/// Reads the pods' packets until `wanted` gives a value for one or `within` passes.
fn wait_for<T>(
    channel: &Channel,
    buf: &mut [u8],
    within: Duration,
    mut wanted: impl FnMut(&[u8]) -> Option<T>,
) -> Result<Option<T>, SourceError> {
    let deadline = Instant::now().checked_add(within);
    while deadline.is_some_and(|deadline| Instant::now() < deadline) {
        if let Some(found) = receive(channel, buf)?.and_then(&mut wanted) {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// One packet from the pods, or none when the poll ran out or a signal ended it; a packet too long for the buffer is none.
fn receive<'b>(channel: &Channel, buf: &'b mut [u8]) -> Result<Option<&'b [u8]>, SourceError> {
    match channel.recv(buf) {
        Ok(Received::Sdu(len)) => Ok(buf.get(..len)),
        Ok(Received::TimedOut | Received::Truncated { .. })
        | Err(ChannelError::Recv(Errno::INTR)) => Ok(None),
        Ok(Received::Closed) => Err(SourceError::PeerClosed),
        Err(error) => Err(SourceError::Channel(error)),
    }
}

/// Why the uplink was let go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The seconds passed, a signal came, or the node could not be opened.
    Asked,
    /// The pods sent the stopped marker themselves.
    ByPods,
}

/// Hands each audio packet to the chain as it lands, printing a status line a second, until `until`, a signal, or the pods' own stop.
fn feed(
    channel: &Channel,
    buf: &mut [u8],
    chain: &mut Chain,
    served: &Served,
    out: &mut dyn Write,
    interrupted: &AtomicBool,
    until: Option<Instant>,
) -> Result<Ended, SourceError> {
    let mut status_due = STATUS_EVERY;
    loop {
        if interrupted.load(Ordering::SeqCst) || until.is_some_and(|until| Instant::now() >= until)
        {
            return Ok(Ended::Asked);
        }
        match served.gone.try_recv() {
            Err(TryRecvError::Empty) => {}
            // The sender lives in the node's callbacks, which end only with the node.
            Ok(()) | Err(TryRecvError::Disconnected) => return Err(SourceError::Lost),
        }
        if served.since.elapsed() >= status_due {
            served.print_status(out)?;
            status_due = status_due.saturating_add(STATUS_EVERY);
        }
        let Some(sdu) = receive(channel, buf)? else {
            continue;
        };
        if uplink::state::parse(sdu) == Some(UplinkState::Stopped) {
            return Ok(Ended::ByPods);
        }
        if let Ok(Some(UplinkPacket::Audio(packet))) = UplinkPacket::parse(sdu) {
            let units = packet.units().map(|unit| (unit.stamp, unit.data));
            chain.sdu(served.since.elapsed(), units);
        }
    }
}

/// What the restore did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Restored {
    Done,
    /// No sink of the device was listed, so there was nothing to suspend.
    NoSink,
}

/// How the stop of the uplink went, then how the restore did.
type Stopped = (Result<(), SourceError>, Result<Restored, SourceError>);

/// An uplink the pods said runs: dropping it on any way out, an unwind included, sends STOP and restores.
struct Running<'a> {
    channel: &'a Channel,
    peer: BdAddr,
    pending: bool,
}

impl Running<'_> {
    /// Sends STOP unless the pods stopped already and waits for their stopped marker, then restores whether or not it came.
    fn finish(&mut self, buf: &mut [u8], by_pods: bool) -> Stopped {
        self.pending = false;
        let stopped = if by_pods {
            Ok(())
        } else {
            stop(self.channel, buf)
        };
        (stopped, restore(self.peer).map_err(SourceError::Restore))
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        if self.pending {
            let mut buf = [0_u8; RECV_BUFFER_LEN];
            let (stopped, restored) = self.finish(&mut buf, false);
            for error in [stopped.err(), restored.err()].into_iter().flatten() {
                crate::report(&format!("{GUARD_SAYS} {error}"));
            }
        }
    }
}

fn stop(channel: &Channel, buf: &mut [u8]) -> Result<(), SourceError> {
    channel.send(&STOP).map_err(SourceError::Channel)?;
    let stopped = wait_for(channel, buf, STOP_ANSWER, |sdu| {
        (uplink::state::parse(sdu) == Some(UplinkState::Stopped)).then_some(())
    })?;
    stopped.ok_or(SourceError::StopUnanswered)
}

/// Suspends the device's sink and resumes it after the hold, the two `pactl` calls the restore is; both name the sink by its index, since its name holds the device's address.
fn restore(peer: BdAddr) -> Result<Restored, String> {
    let listed = pactl(&["list", "short", "sinks"])?;
    let Some(index) = device_sink(&listed, &sink_prefix(peer)) else {
        return Ok(Restored::NoSink);
    };
    let sink = index.to_string();
    pactl(&["suspend-sink", &sink, "1"])?;
    unit::pause(Duration::from_millis(PROBE_RESTORE_HOLD_MS));
    pactl(&["suspend-sink", &sink, "0"])
        .map_err(|error| format!("{error}, so the device's sink stays suspended"))?;
    Ok(Restored::Done)
}

/// The output of one `pactl` call that succeeded; an error names the call by its first word only.
fn pactl(args: &[&str]) -> Result<String, String> {
    let words: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let ran = unit::run("pactl", &words, &[], PACTL_TIMEOUT)?;
    if ran.succeeded() {
        Ok(ran.stdout)
    } else {
        let first = args.first().copied().unwrap_or_default();
        Err(format!("pactl {first} exited with {}", ran.status))
    }
}

/// The index of the first listed sink whose name starts with `prefix` and whose index is a number.
// Sample input: `57\tbluez_output.02_00_00_00_00_01.1\tPipeWire\ts16le 2ch 48000Hz\tSUSPENDED`, whose first field is the index and second the name.
fn device_sink(listed: &str, prefix: &str) -> Option<u32> {
    listed.lines().find_map(|line| {
        let mut fields = line.split('\t');
        let index = fields.next()?;
        let name = fields.next()?;
        name.starts_with(prefix).then(|| index.parse().ok())?
    })
}

#[cfg(test)]
mod tests;
