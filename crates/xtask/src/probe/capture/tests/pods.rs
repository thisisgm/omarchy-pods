//! Fake pods on the far end of a socket pair, and the harness that runs the loop against them.

use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::packets::{
    FEATURES_ACK_PREFIX, HANDSHAKE, HANDSHAKE_ACK_PREFIX, REQUEST_NOTIFICATIONS,
    SET_SPECIFIC_FEATURES,
};
use pods_proto::sdulog::{self, Direction, Record};
use pods_proto::uplink::{AccessUnit, MicSide, OPAQUE_LEN, START, STOP, encode_audio};
use pods_sys::l2cap::{self, Channel, Received};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

use super::super::super::script::{Event, Opening, Script};
use super::super::{Outcome, Outputs, Timing, run_with};

/// A wait that has not ended by then is a hang, and the test fails.
pub(super) const DEADLINE: Duration = Duration::from_secs(30);
/// Longer than a passing test takes, so a wait of this length never runs out in one.
pub(super) const PATIENT_NS: u64 = 20_000_000_000;
/// The tail the tests keep the channel open for after STOP.
pub(super) const TAIL_NS: u64 = 40_000_000;
/// How long the fake pods wait for a packet before they send the next of their own.
const PODS_POLL: Duration = Duration::from_millis(2);
/// Larger than any packet the loop sends, so the fake pods never cut one short.
const PODS_BUFFER_LEN: usize = 8192;
/// The two byte unit the tests of `pods_proto::uplink` call silent; the loop logs it and never decodes it.
const SILENT_UNIT: [u8; 2] = [0x00, 0x00];
/// How many units the pods are reported to put in one audio SDU.
const UNITS_PER_SDU: u32 = 4;
/// Both buds in the ear: the header of `pods_proto::ear`, then its placement byte 00 for each bud.
pub(super) const EAR_PACKET: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x00];
/// How a log line of an audio SDU from the pods begins after its stamp: the mark of `pods_proto::sdulog` for a packet from the pods, then the header of `pods_proto::uplink` with subtype 1.
const AUDIO_LINE: &[u8] = b" < 0400040058000100";
/// What follows the prefix in an acknowledgement here: two bytes the handshake never reads, so any value serves.
const ACK_REST: [u8; 2] = [0x00, 0x00];
/// The seed of the noise packets, so every run feeds the same bytes.
const NOISE_SEED: [u8; 32] = [0x5a; 32];

/// What the fake pods do; `Pods::default()` answers the handshake, reports the buds in the ear and streams silence from START to STOP.
#[derive(Clone)]
pub(super) struct Pods {
    /// How many of the two acknowledgements they give.
    pub(super) acks: usize,
    /// How long after the notification request the ear packet follows; `None` is never.
    pub(super) ear_after: Option<Duration>,
    /// The microphone side they report after the notification request.
    pub(super) side: Option<MicSide>,
    /// Packets they send as soon as the channel is open.
    pub(super) greeting: Vec<Vec<u8>>,
    /// Packets they send on START in place of the stream, after which they hang up.
    pub(super) burst: Option<Vec<Vec<u8>>>,
    /// The number of audio SDUs after which they raise the interrupted flag.
    pub(super) interrupt_after: Option<u32>,
    /// Whether they hang up once the notification request has arrived.
    pub(super) hang_up_when_ready: bool,
    /// Whether they hang up once STOP has arrived.
    pub(super) hang_up_on_stop: bool,
}

impl Default for Pods {
    fn default() -> Self {
        Self {
            acks: 2,
            ear_after: Some(Duration::ZERO),
            side: None,
            greeting: Vec::new(),
            burst: None,
            interrupt_after: None,
            hang_up_when_ready: false,
            hang_up_on_stop: false,
        }
    }
}

/// One thing the fake pods saw happen, in the order they saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Heard {
    Got(Vec<u8>),
    SentEar,
}

/// What the fake pods saw and sent.
#[derive(Debug, Default)]
pub(super) struct Transcript {
    pub(super) heard: Vec<Heard>,
    /// Every packet that left, in order.
    pub(super) sent: Vec<Vec<u8>>,
}

impl Transcript {
    /// The packets the loop sent, in order.
    pub(super) fn got(&self) -> Vec<&[u8]> {
        let heard = self.heard.iter();
        heard
            .filter_map(|heard| match heard {
                Heard::Got(sdu) => Some(sdu.as_slice()),
                Heard::SentEar => None,
            })
            .collect()
    }

    pub(super) fn count(&self, sdu: &[u8]) -> usize {
        self.got().iter().filter(|got| **got == sdu).count()
    }
}

/// The fake pods' end of the channel.
struct Far {
    channel: Channel,
    transcript: Transcript,
}

impl Far {
    /// Sends one packet and says whether it left; it cannot once the loop has closed the channel.
    fn say(&mut self, sdu: &[u8]) -> bool {
        let left = self.channel.send(sdu).is_ok();
        if left {
            self.transcript.sent.push(sdu.to_vec());
        }
        left
    }
}

/// The audio SDU with this number, counting from 0: four silent units, stamped upward across SDUs.
pub(super) fn audio_sdu(number: u32) -> Vec<u8> {
    let first_stamp = number.saturating_mul(UNITS_PER_SDU);
    let units: Vec<AccessUnit<'_>> = (0..UNITS_PER_SDU)
        .map(|unit| AccessUnit {
            stamp: first_stamp.saturating_add(unit),
            data: &SILENT_UNIT,
        })
        .collect();
    encode_audio(&[0; OPAQUE_LEN], &units).unwrap()
}

/// Bytes from the seeded generator, standing in for a packet nobody has documented.
pub(super) fn noise(len: usize) -> Vec<u8> {
    let mut runner = TestRunner::new_with_rng(
        Config::default(),
        TestRng::from_seed(RngAlgorithm::ChaCha, &NOISE_SEED),
    );
    let bytes = proptest::collection::vec(any::<u8>(), len);
    bytes.new_tree(&mut runner).unwrap().current()
}

/// An acknowledgement as the pods send it: the prefix, then bytes the handshake does not read.
pub(super) fn ack(prefix: &[u8]) -> Vec<u8> {
    [prefix, &ACK_REST].concat()
}

/// Plays the pods until the loop closes the channel, their part says to hang up, or the deadline passes.
fn play(channel: Channel, part: &Pods, interrupted: &AtomicBool) -> Transcript {
    channel.set_recv_timeout(PODS_POLL).unwrap();
    let began = Instant::now();
    let mut far = Far {
        channel,
        transcript: Transcript::default(),
    };
    let mut buf = vec![0_u8; PODS_BUFFER_LEN];
    let mut streaming = false;
    let mut audio_sdus = 0_u32;
    let mut ear_due = None;
    for sdu in &part.greeting {
        far.say(sdu);
    }
    loop {
        if began.elapsed() > DEADLINE {
            break;
        }
        // A loop that closed with a packet of the pods unread shows here as one error, ahead of what it sent before that.
        let Ok(received) = far.channel.recv(&mut buf) else {
            continue;
        };
        let sdu = match received {
            Received::Sdu(len) => buf[..len].to_vec(),
            Received::TimedOut => Vec::new(),
            Received::Closed => break,
            Received::Truncated { full_len } => panic!("the loop sent {full_len} bytes"),
        };
        if !sdu.is_empty() {
            far.transcript.heard.push(Heard::Got(sdu.clone()));
        }
        if sdu == HANDSHAKE && part.acks >= 1 {
            far.say(&ack(&HANDSHAKE_ACK_PREFIX));
        } else if sdu == SET_SPECIFIC_FEATURES && part.acks >= 2 {
            far.say(&ack(&FEATURES_ACK_PREFIX));
        } else if sdu == REQUEST_NOTIFICATIONS {
            if part.hang_up_when_ready {
                break;
            }
            if let Some(side) = part.side {
                far.say(&side.command().encode());
            }
            ear_due = part
                .ear_after
                .and_then(|after| Instant::now().checked_add(after));
        } else if sdu == START {
            if let Some(burst) = &part.burst {
                for packet in burst {
                    far.say(packet);
                }
                break;
            }
            streaming = true;
        } else if sdu == STOP {
            if part.hang_up_on_stop {
                break;
            }
            streaming = false;
        }
        if ear_due.is_some_and(|due| Instant::now() >= due) {
            ear_due = None;
            far.say(&EAR_PACKET);
            far.transcript.heard.push(Heard::SentEar);
        }
        if streaming {
            // A send that fails means the loop has closed; what it sent before that is still queued to read.
            streaming = far.say(&audio_sdu(audio_sdus));
            audio_sdus = audio_sdus.saturating_add(1);
            if part.interrupt_after == Some(audio_sdus) {
                interrupted.store(true, Ordering::SeqCst);
            }
        }
    }
    far.transcript
}

/// Plays the pods on a thread of their own; the transcript arrives when they are done.
pub(super) fn spawn(
    far: Channel,
    part: Pods,
    interrupted: Arc<AtomicBool>,
) -> mpsc::Receiver<Transcript> {
    let (done, result) = mpsc::channel();
    // The send fails only when the test has stopped waiting for it, and a wait that ran out has already failed the test.
    thread::spawn(move || done.send(play(far, &part, &interrupted)));
    result
}

/// Both ends of a channel that no thread plays; what one end sends waits in the other end's queue.
#[expect(
    clippy::disallowed_methods,
    reason = "a test wraps both ends of a socket pair as channels"
)]
pub(super) fn pair() -> (Channel, Channel) {
    let (near, far) = socketpair(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    (l2cap::from_fd(near), l2cap::from_fd(far))
}

/// Every packet waiting in `far`'s queue, in order; an empty queue ends the read at the poll timeout.
pub(super) fn queued(far: &Channel) -> Vec<Vec<u8>> {
    far.set_recv_timeout(PODS_POLL).unwrap();
    let mut buf = vec![0_u8; PODS_BUFFER_LEN];
    let mut packets = Vec::new();
    while let Received::Sdu(len) = far.recv(&mut buf).unwrap() {
        packets.push(buf[..len].to_vec());
    }
    packets
}

#[test]
fn the_fake_pods_read_what_the_loop_sent_before_it_closed_with_a_packet_unread() {
    let (near, far) = pair();
    far.send(&EAR_PACKET).unwrap();
    near.send(&STOP).unwrap();
    drop(near);
    let transcript = spawn(far, Pods::default(), Arc::new(AtomicBool::new(false)));
    let transcript = transcript.recv_timeout(DEADLINE).unwrap();
    assert_eq!(transcript.got(), [&STOP[..]]);
}

/// What the log writer of a run does at the first audio line.
#[derive(Debug, Clone, Copy)]
pub(super) enum LogFault {
    Fails,
    Panics,
}

/// The log writer of a run: keeps what it is given, and may fail or panic at the first audio line.
struct Tape<'c> {
    bytes: Vec<u8>,
    fault: Option<LogFault>,
    faulted_at_ns: Option<u64>,
    now_ns: &'c dyn Fn() -> u64,
}

impl Write for Tape<'_> {
    #[expect(
        clippy::panic_in_result_fn,
        reason = "one fault this writer plays is a panic inside the loop"
    )]
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let audio = data.windows(AUDIO_LINE.len()).any(|at| at == AUDIO_LINE);
        if let Some(fault) = self.fault
            && audio
        {
            self.faulted_at_ns.get_or_insert((self.now_ns)());
            match fault {
                LogFault::Fails => return Err(io::Error::other("the log is full")),
                LogFault::Panics => panic!("the log writer panicked"),
            }
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One run of the loop against fake pods.
pub(super) struct Case {
    pub(super) script: Script,
    pub(super) timing: Timing,
    pub(super) pods: Pods,
    pub(super) log_fault: Option<LogFault>,
    /// The flag is already raised when the run begins.
    pub(super) interrupted: bool,
}

impl Case {
    pub(super) fn new(opening: Opening, events: &[Event]) -> Self {
        Self {
            script: Script {
                opening,
                needs_initial_side: false,
                events: events.to_vec(),
            },
            timing: Timing {
                handshake_step: PATIENT_NS,
                ear_report: PATIENT_NS,
                side_report: PATIENT_NS,
                stop_tail: TAIL_NS,
            },
            pods: Pods::default(),
            log_fault: None,
            interrupted: false,
        }
    }

    /// Runs the loop on one thread and the fake pods on another, and waits for both no longer than the deadline.
    pub(super) fn run(self) -> Report {
        let (near, far) = pair();
        let interrupted = Arc::new(AtomicBool::new(self.interrupted));
        let pods_result = spawn(far, self.pods, Arc::clone(&interrupted));
        let (loop_done, loop_result) = mpsc::channel();
        let (script, timing, log_fault) = (self.script, self.timing, self.log_fault);
        thread::spawn(move || {
            let channel = near;
            let zero = Instant::now();
            let now_ns = move || u64::try_from(zero.elapsed().as_nanos()).unwrap();
            let mut log = Tape {
                bytes: Vec::new(),
                fault: log_fault,
                faulted_at_ns: None,
                now_ns: &now_ns,
            };
            let (mut marks, mut cues) = (Vec::new(), Vec::new());
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                let mut out = Outputs {
                    log: &mut log,
                    marks: &mut marks,
                    cues: &mut cues,
                };
                run_with(&channel, &script, &mut out, &interrupted, &now_ns, &timing)
            }));
            let returned_ns = now_ns();
            drop(channel);
            // As in `spawn`: nobody is left to tell when this send fails.
            loop_done.send(Box::new(Report {
                outcome: outcome.ok(),
                log: String::from_utf8(log.bytes).unwrap(),
                marks: String::from_utf8(marks).unwrap(),
                cues: String::from_utf8(cues).unwrap(),
                returned_ns,
                faulted_at_ns: log.faulted_at_ns,
                pods: Transcript::default(),
            }))
        });
        let report = loop_result.recv_timeout(DEADLINE);
        let mut report = report.expect("the loop did not return before the deadline");
        let pods = pods_result.recv_timeout(DEADLINE);
        report.pods = pods.expect("the fake pods did not finish before the deadline");
        *report
    }
}

/// What one run left behind.
pub(super) struct Report {
    /// `None` when the loop panicked.
    pub(super) outcome: Option<Outcome>,
    pub(super) log: String,
    pub(super) marks: String,
    pub(super) cues: String,
    /// The loop's clock when `run_with` had returned or unwound.
    pub(super) returned_ns: u64,
    /// The loop's clock when the log writer failed or panicked.
    pub(super) faulted_at_ns: Option<u64>,
    pub(super) pods: Transcript,
}

impl Report {
    pub(super) fn outcome(&self) -> Outcome {
        self.outcome.expect("the loop panicked")
    }

    /// Every line of the log, which must all read back.
    pub(super) fn records(&self) -> Vec<Record> {
        let lines = self.log.lines();
        lines
            .map(|line| sdulog::parse_line(line).unwrap())
            .collect()
    }

    pub(super) fn logged(&self, direction: Direction) -> Vec<Vec<u8>> {
        let records = self.records().into_iter();
        records
            .filter(|record| record.direction == direction)
            .map(|record| record.sdu)
            .collect()
    }

    /// The stamp and the text of every mark of one kind.
    // Sample input: "1203456789\tgate\tear", a stamp in nanoseconds, a kind and a text, separated by tabs.
    pub(super) fn marks(&self, kind: &str) -> Vec<(u64, &str)> {
        let lines = self.marks.lines();
        lines
            .filter_map(|line| {
                let mut fields = line.splitn(3, '\t');
                let stamp = fields.next().unwrap().parse().unwrap();
                let found = fields.next().unwrap();
                let text = fields.next().unwrap();
                (found == kind).then_some((stamp, text))
            })
            .collect()
    }

    /// The kind of every mark, in the order the loop wrote them.
    // Sample input: "1203456789\tgate\tear", of which the second field is the kind.
    pub(super) fn kinds(&self) -> Vec<&str> {
        let lines = self.marks.lines();
        lines.map(|line| line.split('\t').nth(1).unwrap()).collect()
    }

    /// The text of the only mark of one kind.
    pub(super) fn mark(&self, kind: &str) -> &str {
        let marks = self.marks(kind);
        assert_eq!(marks.len(), 1, "{kind} marks in:\n{}", self.marks);
        marks[0].1
    }

    /// The log stamp of the only time the loop sent `sdu`.
    pub(super) fn sent_at_ns(&self, sdu: &[u8]) -> u64 {
        let records = self.records();
        let mut sent = records
            .iter()
            .filter(|record| record.direction == Direction::ToPods && record.sdu == sdu);
        let at_ns = sent.next().expect("the packet is not in the log").at_ns;
        assert!(sent.next().is_none(), "the packet is in the log twice");
        at_ns
    }

    /// Whether the pods got STOP once, after START, with only a side command allowed to follow it.
    pub(super) fn stop_followed_start(&self) -> bool {
        let got = self.pods.got();
        let start = got.iter().position(|sdu| *sdu == START);
        let stop = got.iter().position(|sdu| *sdu == STOP);
        let after_stop = stop.map_or(&[][..], |stop| &got[stop.saturating_add(1)..]);
        let side_only = after_stop.iter().all(|sdu| {
            ControlCommand::parse(sdu).is_ok_and(|command| command.id == ControlId::MIC_SIDE)
        });
        start.is_some() && start < stop && self.pods.count(&STOP) == 1 && side_only
    }
}
