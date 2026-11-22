//! Fake pods on the far end of a socket pair, and the harness that runs the loop against them.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::packets::{
    FEATURES_ACK_PREFIX, HANDSHAKE, HANDSHAKE_ACK_PREFIX, REQUEST_NOTIFICATIONS,
    SET_SPECIFIC_FEATURES,
};
use pods_proto::sdulog::{self, Direction, Record};
use pods_proto::uplink::{AccessUnit, MicSide, OPAQUE_LEN, START, STOP, encode_audio};
use pods_sys::l2cap::{self, Channel, ChannelError, Received, wait_any};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use rustix::io::Errno;
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

use super::super::super::script::{Event, Opening, Script};
use super::super::{Bearer, EndReason, Links, OpenAtt, Outcome, Outputs, Timing, run_with};
use super::att_pods::{AttPods, AttState};

/// A wait that has not ended by then is a hang, and the test fails.
pub(super) const DEADLINE: Duration = Duration::from_secs(30);
/// Longer than a passing test takes, so a wait of this length never runs out in one.
pub(super) const PATIENT_NS: u64 = 20_000_000_000;
/// The same wait as a script offset.
pub(super) const PATIENT_MS: u32 = 20_000;
/// The tail the tests keep the channel open for after STOP.
pub(super) const TAIL_NS: u64 = 40_000_000;
/// How long a connect of the attribute channel waits for the handshake to end before the fake pods refuse it.
const ORDER_WAIT: Duration = Duration::from_secs(2);
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
/// How a line of an audio notification begins after its stamp in the log of the attribute channel: the same mark, then the notification opcode, handle 0x000d and the audio marker.
const ATT_AUDIO_LINE: &[u8] = b" < 1b0d000100";
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
    /// Packets they send once STOP has arrived.
    pub(super) parting: Vec<Vec<u8>>,
    /// What they do on the attribute channel.
    pub(super) att: AttPods,
    /// Set, and its waiters woken, once the notification request has arrived, which is where the handshake ends.
    pub(super) handshaken: Arc<(Mutex<bool>, Condvar)>,
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
            parting: Vec::new(),
            att: AttPods::default(),
            handshaken: Arc::default(),
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
    /// Every packet that left on the control channel, in order.
    pub(super) sent: Vec<Vec<u8>>,
    /// Every packet the loop sent on the attribute channel, in order.
    pub(super) att_got: Vec<Vec<u8>>,
    /// Every packet that left on the attribute channel, in order.
    pub(super) att_sent: Vec<Vec<u8>>,
    /// How many of `att_got` had arrived when the loop closed the attribute channel and they turned to the fresh one.
    pub(super) att_fresh_from: Option<usize>,
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

/// The fake pods' ends of the channels; one that is `None` was never opened or has been hung up.
struct Far {
    accessory: Option<Channel>,
    att: Option<Channel>,
    /// Their end of the attribute channel the loop may open once it has closed the first.
    fresh_att: Option<Channel>,
    transcript: Transcript,
}

impl Far {
    /// Sends one packet on the control channel and says whether it left; it cannot once the loop has closed the channel.
    fn say(&mut self, sdu: &[u8]) -> bool {
        let left = self.accessory.as_ref();
        let left = left.is_some_and(|channel| channel.send(sdu).is_ok());
        if left {
            self.transcript.sent.push(sdu.to_vec());
        }
        left
    }

    /// Sends one packet on the attribute channel and says whether it left.
    fn say_att(&mut self, pdu: &[u8]) -> bool {
        let left = self.att.as_ref();
        let left = left.is_some_and(|channel| channel.send(pdu).is_ok());
        if left {
            self.transcript.att_sent.push(pdu.to_vec());
        }
        left
    }

    /// Sends what is due of the stream on the attribute channel, and does what their part says follows that packet.
    fn stream_on(&mut self, stream: &mut AttState, part: &AttPods, interrupted: &AtomicBool) {
        if let Some(reply) = stream.late_reply() {
            self.say_att(&reply);
        }
        let Some(packet) = stream.next_packet() else {
            return;
        };
        if !self.say_att(&packet) {
            stream.cut();
            return;
        }
        stream.after_burst = stream.after_burst.saturating_add(1);
        let sent = Some(stream.after_burst);
        if part.interrupt_after == sent {
            interrupted.store(true, Ordering::SeqCst);
        }
        if part.hang_up_after == sent {
            self.att = None;
        }
        if part.hang_up_control_after == sent {
            self.accessory = None;
        }
    }

    /// The next packet from the loop and its channel, waiting no longer than the poll; a channel the loop closed is forgotten, and `stream` ends with the attribute channel.
    fn hear(&mut self, stream: &mut AttState, buf: &mut [u8]) -> Option<(Bearer, Vec<u8>)> {
        let ends = [
            (Bearer::Accessory, self.accessory.as_ref()),
            (Bearer::Att, self.att.as_ref()),
        ];
        let open: Vec<(Bearer, &Channel)> = ends
            .into_iter()
            .filter_map(|(bearer, channel)| Some((bearer, channel?)))
            .collect();
        let channels: Vec<&Channel> = open.iter().map(|(_, channel)| *channel).collect();
        let (bearer, channel) = open[wait_any(&channels, PODS_POLL).ok()??];
        // A loop that closed with a packet of the pods unread shows here as one error, ahead of what it sent before that.
        match channel.recv(buf).ok()? {
            Received::Sdu(len) => Some((bearer, buf[..len].to_vec())),
            Received::TimedOut => None,
            Received::Closed => {
                match bearer {
                    Bearer::Accessory => self.accessory = None,
                    Bearer::Att => {
                        // A fresh channel has seen no start write, so nothing of the stream moves to it.
                        stream.cut();
                        self.att = self.fresh_att.take();
                        if self.att.is_some() {
                            self.transcript.att_fresh_from = Some(self.transcript.att_got.len());
                        }
                    }
                }
                None
            }
            Received::Truncated { full_len } => panic!("the loop sent {full_len} bytes"),
        }
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

/// Whether the handshake ended within `ORDER_WAIT`, which a connect of the attribute channel waits for.
pub(super) fn handshake_ended(handshaken: &(Mutex<bool>, Condvar)) -> bool {
    let (done, waiters) = handshaken;
    let waited = waiters.wait_timeout_while(done.lock().unwrap(), ORDER_WAIT, |done| !*done);
    !waited.unwrap().1.timed_out()
}

/// An acknowledgement as the pods send it: the prefix, then bytes the handshake does not read.
pub(super) fn ack(prefix: &[u8]) -> Vec<u8> {
    [prefix, &ACK_REST].concat()
}

/// Plays the pods until the loop closes every channel, their part says to hang up, or the deadline passes.
fn play(mut far: Far, part: &Pods, interrupted: &AtomicBool) -> Transcript {
    for channel in [&far.accessory, &far.att, &far.fresh_att]
        .into_iter()
        .flatten()
    {
        channel.set_recv_timeout(PODS_POLL).unwrap();
    }
    let began = Instant::now();
    let mut buf = vec![0_u8; PODS_BUFFER_LEN];
    let mut streaming = false;
    let mut audio_sdus = 0_u32;
    let mut ear_due = None;
    let mut on_att = AttState::default();
    for sdu in &part.greeting {
        far.say(sdu);
    }
    for pdu in &part.att.greeting {
        far.say_att(pdu);
    }
    if part.att.hang_up {
        far.att = None;
    }
    while began.elapsed() <= DEADLINE && (far.accessory.is_some() || far.att.is_some()) {
        let sdu = match far.hear(&mut on_att, &mut buf) {
            Some((Bearer::Accessory, sdu)) => sdu,
            Some((Bearer::Att, pdu)) => {
                for reply in on_att.answer(&part.att, &pdu) {
                    far.say_att(&reply);
                }
                far.transcript.att_got.push(pdu);
                Vec::new()
            }
            None => Vec::new(),
        };
        if !sdu.is_empty() {
            far.transcript.heard.push(Heard::Got(sdu.clone()));
        }
        if sdu == HANDSHAKE && part.acks >= 1 {
            far.say(&ack(&HANDSHAKE_ACK_PREFIX));
        } else if sdu == SET_SPECIFIC_FEATURES && part.acks >= 2 {
            far.say(&ack(&FEATURES_ACK_PREFIX));
        } else if sdu == REQUEST_NOTIFICATIONS {
            let (done, waiters) = &*part.handshaken;
            *done.lock().unwrap() = true;
            waiters.notify_all();
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
            for packet in &part.parting {
                far.say(packet);
            }
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
        far.stream_on(&mut on_att, &part.att, interrupted);
    }
    far.transcript
}

/// Plays the pods on the control channel alone, on a thread of their own; the transcript arrives when they are done.
pub(super) fn spawn(
    far: Channel,
    part: Pods,
    interrupted: Arc<AtomicBool>,
) -> mpsc::Receiver<Transcript> {
    spawn_on(Some(far), None, part, interrupted)
}

/// Plays the pods on the channels given, with no fresh attribute channel to turn to.
pub(super) fn spawn_on(
    accessory: Option<Channel>,
    att: Option<Channel>,
    part: Pods,
    interrupted: Arc<AtomicBool>,
) -> mpsc::Receiver<Transcript> {
    spawn_with_fresh(accessory, [att, None], part, interrupted)
}

/// Plays the pods on the channels given, on a thread of their own; `att` is their end of the attribute channel and of the fresh one that may follow it, and the transcript arrives when they are done.
pub(super) fn spawn_with_fresh(
    accessory: Option<Channel>,
    att: [Option<Channel>; 2],
    part: Pods,
    interrupted: Arc<AtomicBool>,
) -> mpsc::Receiver<Transcript> {
    let (done, result) = mpsc::channel();
    let [att, fresh_att] = att;
    let far = Far {
        accessory,
        att,
        fresh_att,
        transcript: Transcript::default(),
    };
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

/// The log writer of a run: keeps what it is given, and may fail or panic at the first line that holds `audio_line`.
struct Tape<'c> {
    bytes: Vec<u8>,
    audio_line: &'static [u8],
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
        let mut windows = data.windows(self.audio_line.len());
        let audio = windows.any(|at| at == self.audio_line);
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
    /// What the writer of the log of the attribute channel does at the first audio notification.
    pub(super) att_log_fault: Option<LogFault>,
    /// The flag is already raised when the run begins.
    pub(super) interrupted: bool,
    /// The channels the run can have; a script that needs one it lacks is the caller's mistake.
    pub(super) wired: [Bearer; 2],
    /// Whether a second connect of the attribute channel is answered, as it is for a fresh one after the first was closed.
    pub(super) fresh_att: bool,
}

/// The control channel alone, written twice because a run holds one or two channels.
pub(super) const CONTROL_ONLY: [Bearer; 2] = [Bearer::Accessory; 2];
/// The attribute channel alone.
pub(super) const ATT_ONLY: [Bearer; 2] = [Bearer::Att; 2];
/// Both channels.
pub(super) const BOTH_CHANNELS: [Bearer; 2] = [Bearer::Accessory, Bearer::Att];

impl Case {
    pub(super) fn new(opening: Opening, events: &[Event]) -> Self {
        Self {
            script: Script {
                opening,
                needs_initial_side: false,
                unchecked: false,
                events: events.to_vec(),
            },
            timing: Timing {
                handshake_step: PATIENT_NS,
                ear_report: PATIENT_NS,
                side_report: PATIENT_NS,
                handle_walk_ms: PATIENT_MS,
                stop_tail: TAIL_NS,
            },
            pods: Pods::default(),
            log_fault: None,
            att_log_fault: None,
            interrupted: false,
            wired: CONTROL_ONLY,
            fresh_att: false,
        }
    }

    /// Runs the loop on one thread and the fake pods on another, and waits for both no longer than the deadline.
    pub(super) fn run(self) -> Report {
        let wire = |bearer| self.wired.contains(&bearer).then(pair).unzip();
        let (near, far) = wire(Bearer::Accessory);
        let (near_att, far_att) = wire(Bearer::Att);
        let (near_fresh, far_fresh) = self.fresh_att.then(pair).unzip();
        let interrupted = Arc::new(AtomicBool::new(self.interrupted));
        let handshaken = Arc::clone(&self.pods.handshaken);
        let far_atts = [far_att, far_fresh];
        let pods_result = spawn_with_fresh(far, far_atts, self.pods, Arc::clone(&interrupted));
        let (loop_done, loop_result) = mpsc::channel();
        let (script, timing, log_fault) = (self.script, self.timing, self.log_fault);
        let att_log_fault = self.att_log_fault;
        thread::spawn(move || {
            let att_lines: VecDeque<Channel> =
                [near_att, near_fresh].into_iter().flatten().collect();
            let (att_wired, after_handshake) = (!att_lines.is_empty(), near.is_some());
            let (att_lines, att_dials) = (RefCell::new(att_lines), Cell::new(0_usize));
            let refused = ChannelError::Connect(Errno::CONNREFUSED);
            let open_att = || {
                att_dials.set(att_dials.get().saturating_add(1));
                // A Mac was seen to finish the handshake first, so the fake pods refuse a connect that comes before its end.
                if after_handshake && !handshake_ended(&handshaken) {
                    return Err(refused);
                }
                att_lines.borrow_mut().pop_front().ok_or(refused)
            };
            let open_att: OpenAtt<'_> = &open_att;
            let zero = Instant::now();
            let now_ns = move || u64::try_from(zero.elapsed().as_nanos()).unwrap();
            let mut log = Tape {
                bytes: Vec::new(),
                audio_line: AUDIO_LINE,
                fault: log_fault,
                faulted_at_ns: None,
                now_ns: &now_ns,
            };
            let mut att_log = Tape {
                bytes: Vec::new(),
                audio_line: ATT_AUDIO_LINE,
                fault: att_log_fault,
                faulted_at_ns: None,
                now_ns: &now_ns,
            };
            let (mut handles, mut marks, mut cues) = (Vec::new(), Vec::new(), Vec::new());
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                let links = Links {
                    accessory: near.as_ref(),
                    att: att_wired.then_some(open_att),
                };
                let mut out = Outputs {
                    log: &mut log,
                    att_log: Some(&mut att_log),
                    handles: Some(&mut handles),
                    marks: &mut marks,
                    cues: &mut cues,
                };
                run_with(&links, &script, &mut out, &interrupted, &now_ns, &timing)
            }));
            let returned_ns = now_ns();
            drop((near, att_lines));
            // As in `spawn_on`: nobody is left to tell when this send fails.
            loop_done.send(Box::new(Report {
                outcome: outcome.ok(),
                log: String::from_utf8(log.bytes).unwrap(),
                att_log: String::from_utf8(att_log.bytes).unwrap(),
                handles: String::from_utf8(handles).unwrap(),
                marks: String::from_utf8(marks).unwrap(),
                cues: String::from_utf8(cues).unwrap(),
                returned_ns,
                faulted_at_ns: log.faulted_at_ns.or(att_log.faulted_at_ns),
                att_dials: att_dials.get(),
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
    /// The log of the attribute channel.
    pub(super) att_log: String,
    /// The list of the pods' attribute table.
    pub(super) handles: String,
    pub(super) marks: String,
    pub(super) cues: String,
    /// The loop's clock when `run_with` had returned or unwound.
    pub(super) returned_ns: u64,
    /// The loop's clock when the log writer failed or panicked.
    pub(super) faulted_at_ns: Option<u64>,
    /// How many times the loop asked for the attribute channel to be opened.
    pub(super) att_dials: usize,
    pub(super) pods: Transcript,
}

impl Report {
    pub(super) fn outcome(&self) -> Outcome {
        self.outcome.clone().expect("the loop panicked")
    }

    /// Why the run ended; `None` when the loop panicked.
    pub(super) fn end(&self) -> Option<EndReason> {
        let outcome = self.outcome.as_ref();
        outcome.map(|outcome| outcome.end.clone())
    }

    /// Every line of the log, which must all read back.
    pub(super) fn records(&self) -> Vec<Record> {
        let lines = self.log.lines();
        lines
            .map(|line| sdulog::parse_line(line).unwrap())
            .collect()
    }

    /// Every line of the log of the attribute channel, which must all read back.
    pub(super) fn att_records(&self) -> Vec<Record> {
        let lines = self.att_log.lines();
        lines
            .map(|line| sdulog::parse_line(line).unwrap())
            .collect()
    }

    /// The packets of the log of the attribute channel that went one way, in order.
    pub(super) fn att_logged(&self, direction: Direction) -> Vec<Vec<u8>> {
        let records = self.att_records().into_iter();
        records
            .filter(|record| record.direction == direction)
            .map(|record| record.sdu)
            .collect()
    }

    pub(super) fn logged(&self, direction: Direction) -> Vec<Vec<u8>> {
        let records = self.records().into_iter();
        records
            .filter(|record| record.direction == direction)
            .map(|record| record.sdu)
            .collect()
    }

    /// The stamp and the text of every mark of one kind, as `1203456789\tgate\tear` is a stamp in nanoseconds, a kind and a text.
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

    /// The text of every mark of one kind, in the order the loop wrote them.
    pub(super) fn texts(&self, kind: &str) -> Vec<&str> {
        let marks = self.marks(kind).into_iter();
        marks.map(|(_, text)| text).collect()
    }

    /// The kind of every mark, in the order the loop wrote them: the second field of `1203456789\tgate\tear`.
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
