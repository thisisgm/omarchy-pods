//! Fake pods on the far end of a channel, on their own thread; unit tests and command tests run against them.

use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use pods_proto::packets::{
    FEATURES_ACK_PREFIX, HANDSHAKE, HANDSHAKE_ACK_PREFIX, REQUEST_NOTIFICATIONS,
    SET_SPECIFIC_FEATURES,
};
use pods_proto::uplink::{AccessUnit, MicSide, OPAQUE_LEN, START, STOP, encode_audio};
use pods_sys::l2cap::{self, Channel, Received, wait_any};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

use super::att_pods::{AttPods, AttState};

/// A wait that has not ended by then is a hang, and the test fails.
pub(super) const DEADLINE: Duration = Duration::from_secs(30);
/// How long a connect of the attribute channel waits for the handshake to end before the fake pods refuse it.
const ORDER_WAIT: Duration = Duration::from_secs(2);
/// How long the fake pods wait for a packet before they send the next of their own.
const PODS_POLL: Duration = Duration::from_millis(2);
/// Larger than any packet the loop sends, so the fake pods never cut one short.
const PODS_BUFFER_LEN: usize = 8192;
/// The two byte unit the tests of `pods_proto::uplink` call silent; the loop logs it and never decodes it.
pub(super) const SILENT_UNIT: [u8; 2] = [0x00, 0x00];
const UNITS_PER_SDU: u32 = 4;
/// Both buds in the ear: the header of `pods_proto::ear`, then its placement byte 00 for each bud.
pub(super) const EAR_PACKET: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x00];
/// What follows the prefix in an acknowledgement here: two bytes the handshake never reads, so any value serves.
const ACK_REST: [u8; 2] = [0x00, 0x00];
/// The seed of the noise packets, so every run feeds the same bytes.
const NOISE_SEED: [u8; 32] = [0x5a; 32];

/// What a test runs at a point of the fake pods' play, on their thread.
pub(super) type Hook = Arc<dyn Fn() + Send + Sync>;

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
    pub(super) hang_up_on_stop: bool,
    /// How many audio SDUs they stream before they go quiet; `None` is until STOP.
    pub(super) stream_for: Option<u32>,
    /// The body of every unit they stream; `None` is the silent unit.
    pub(super) unit: Option<Vec<u8>>,
    /// Packets they send on START, ahead of the stream.
    pub(super) starting: Vec<Vec<u8>>,
    /// Called once START has arrived, before the starting packets go.
    pub(super) on_start: Option<Hook>,
    /// Called once STOP has arrived, before the parting packets go.
    pub(super) on_stop: Option<Hook>,
    /// Packets they send once STOP has arrived.
    pub(super) parting: Vec<Vec<u8>>,
    pub(super) att: AttPods,
    pub(super) deadline: Duration,
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
            stream_for: None,
            unit: None,
            starting: Vec::new(),
            on_start: None,
            on_stop: None,
            parting: Vec::new(),
            att: AttPods::default(),
            deadline: DEADLINE,
            handshaken: Arc::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Heard {
    Got(Vec<u8>),
    SentEar,
}

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

#[derive(Clone, Copy)]
enum End {
    Control,
    Att,
}

/// A channel that is `None` was never opened or has been hung up.
struct Far {
    accessory: Option<Channel>,
    att: Option<Channel>,
    /// Where their end of the attribute channel arrives each time the loop dials it; `None` once no dial can come.
    att_dial: Option<mpsc::Receiver<Channel>>,
    /// Whether a dial was taken already, which makes the next one the fresh channel.
    att_taken: bool,
    transcript: Transcript,
}

impl Far {
    fn new(accessory: Option<Channel>, att_dial: mpsc::Receiver<Channel>) -> Self {
        Self {
            accessory,
            att: None,
            att_dial: Some(att_dial),
            att_taken: false,
            transcript: Transcript::default(),
        }
    }

    /// Whether anything is left to play: an open channel, or an attribute dial still due even once the control channel is closed.
    fn open(&self) -> bool {
        self.accessory.is_some() || self.att.is_some() || self.att_dial.is_some()
    }

    /// Takes their end of the attribute channel once the loop has dialled it: the first gets the greeting their part has for it, a later one is the fresh channel.
    fn pick_up_att(&mut self, part: &AttPods) {
        if self.att.is_some() {
            return;
        }
        let channel = match self.att_dial.as_ref().map(mpsc::Receiver::try_recv) {
            Some(Ok(channel)) => channel,
            // A dialler gone without dialling never will.
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.att_dial = None;
                return;
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => return,
        };
        channel.set_recv_timeout(PODS_POLL).unwrap();
        self.att = Some(channel);
        if mem::replace(&mut self.att_taken, true) {
            self.transcript.att_fresh_from = Some(self.transcript.att_got.len());
            return;
        }
        for pdu in &part.greeting {
            self.say_att(pdu);
        }
        if part.hang_up {
            self.att = None;
        }
    }

    /// Says whether the packet left; it cannot once the loop has closed the channel.
    fn say(&mut self, sdu: &[u8]) -> bool {
        let left = self.accessory.as_ref();
        let left = left.is_some_and(|channel| channel.send(sdu).is_ok());
        if left {
            self.transcript.sent.push(sdu.to_vec());
        }
        left
    }

    fn say_att(&mut self, pdu: &[u8]) -> bool {
        let left = self.att.as_ref();
        let left = left.is_some_and(|channel| channel.send(pdu).is_ok());
        if left {
            self.transcript.att_sent.push(pdu.to_vec());
        }
        left
    }

    fn stream_on(&mut self, stream: &mut AttState, part: &AttPods, interrupted: &AtomicBool) {
        if let Some(reply) = stream.late_reply() {
            self.say_att(&reply);
        }
        let Some((packet, sent)) = stream.next_packet(part) else {
            return;
        };
        if !self.say_att(&packet) {
            stream.cut();
            return;
        }
        if sent.is_none() {
            return;
        }
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

    /// Waits no longer than the poll; a closed channel is forgotten, and `stream` ends with the attribute one.
    fn hear(&mut self, stream: &mut AttState, buf: &mut [u8]) -> Option<(End, Vec<u8>)> {
        let ends = [
            (End::Control, self.accessory.as_ref()),
            (End::Att, self.att.as_ref()),
        ];
        let open: Vec<(End, &Channel)> = ends
            .into_iter()
            .filter_map(|(end, channel)| Some((end, channel?)))
            .collect();
        let channels: Vec<&Channel> = open.iter().map(|(_, channel)| *channel).collect();
        let (end, channel) = open[wait_any(&channels, PODS_POLL).ok()??];
        // A loop that closed with a packet of the pods unread shows here as one error, ahead of what it sent before that.
        match channel.recv(buf).ok()? {
            Received::Sdu(len) => Some((end, buf[..len].to_vec())),
            Received::TimedOut => None,
            Received::Closed => {
                match end {
                    End::Control => self.accessory = None,
                    End::Att => {
                        // A fresh channel has seen no start write, so nothing of the stream moves to it.
                        stream.cut();
                        self.att = None;
                    }
                }
                None
            }
            Received::Truncated { full_len } => panic!("the loop sent {full_len} bytes"),
        }
    }
}

/// The audio SDU with this number, counting from 0: four units of `body`, stamped upward across SDUs.
pub(super) fn audio_sdu(number: u32, body: &[u8]) -> Vec<u8> {
    let first_stamp = number.saturating_mul(UNITS_PER_SDU);
    let units: Vec<AccessUnit<'_>> = (0..UNITS_PER_SDU)
        .map(|unit| AccessUnit {
            stamp: first_stamp.saturating_add(unit),
            data: body,
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

fn play(mut far: Far, part: &Pods, interrupted: &AtomicBool) -> Transcript {
    if let Some(channel) = &far.accessory {
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
    far.pick_up_att(&part.att);
    while began.elapsed() <= part.deadline && far.open() {
        let sdu = match far.hear(&mut on_att, &mut buf) {
            Some((End::Control, sdu)) => sdu,
            Some((End::Att, pdu)) => {
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
            if let Some(on_start) = &part.on_start {
                on_start();
            }
            if let Some(burst) = &part.burst {
                for packet in burst {
                    far.say(packet);
                }
                break;
            }
            for packet in &part.starting {
                far.say(packet);
            }
            streaming = true;
        } else if sdu == STOP {
            if let Some(on_stop) = &part.on_stop {
                on_stop();
            }
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
        let quiet = part.stream_for.is_some_and(|last| audio_sdus >= last);
        if streaming && !quiet {
            // A send that fails means the loop has closed; what it sent before that is still queued to read.
            let body = part.unit.as_deref().unwrap_or(&SILENT_UNIT);
            streaming = far.say(&audio_sdu(audio_sdus, body));
            audio_sdus = audio_sdus.saturating_add(1);
            if part.interrupt_after == Some(audio_sdus) {
                interrupted.store(true, Ordering::SeqCst);
            }
        }
        far.stream_on(&mut on_att, &part.att, interrupted);
        far.pick_up_att(&part.att);
    }
    far.transcript
}

pub(super) fn spawn(
    far: Channel,
    part: Pods,
    interrupted: Arc<AtomicBool>,
) -> mpsc::Receiver<Transcript> {
    spawn_on(Some(far), None, part, interrupted)
}

pub(super) fn spawn_on(
    accessory: Option<Channel>,
    att: Option<Channel>,
    part: Pods,
    interrupted: Arc<AtomicBool>,
) -> mpsc::Receiver<Transcript> {
    spawn_dialled(accessory, dialled([att]), part, interrupted)
}

pub(super) fn spawn_dialled(
    accessory: Option<Channel>,
    att_dial: mpsc::Receiver<Channel>,
    part: Pods,
    interrupted: Arc<AtomicBool>,
) -> mpsc::Receiver<Transcript> {
    play_apart(Far::new(accessory, att_dial), part, interrupted)
}

/// Plays the pods on the channels given; `att` is their end of the attribute channel and of the fresh one that may follow it.
pub(super) fn spawn_with_fresh(
    accessory: Option<Channel>,
    att: [Option<Channel>; 2],
    part: Pods,
    interrupted: Arc<AtomicBool>,
) -> mpsc::Receiver<Transcript> {
    spawn_dialled(accessory, dialled(att), part, interrupted)
}

fn dialled<const N: usize>(att: [Option<Channel>; N]) -> mpsc::Receiver<Channel> {
    let (dial, att_dial) = mpsc::channel();
    for att in att.into_iter().flatten() {
        dial.send(att).unwrap();
    }
    att_dial
}

fn play_apart(far: Far, part: Pods, interrupted: Arc<AtomicBool>) -> mpsc::Receiver<Transcript> {
    let (done, result) = mpsc::channel();
    // The send fails only when the test has stopped waiting for it, and a wait that ran out has already failed the test.
    thread::spawn(move || done.send(play(far, &part, &interrupted)));
    result
}

/// Both ends of a channel no thread plays; what one end sends waits in the other's queue.
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

/// An empty queue ends the read at the poll timeout.
pub(super) fn queued(far: &Channel) -> Vec<Vec<u8>> {
    far.set_recv_timeout(PODS_POLL).unwrap();
    let mut buf = vec![0_u8; PODS_BUFFER_LEN];
    let mut packets = Vec::new();
    while let Received::Sdu(len) = far.recv(&mut buf).unwrap() {
        packets.push(buf[..len].to_vec());
    }
    packets
}
