//! The harness that runs the loop against the fake pods and reads back what the run left.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use pods_proto::control::{ControlCommand, ControlId};
use pods_proto::sdulog::{self, Direction, Record};
use pods_proto::uplink::{START, STOP};
use pods_sys::l2cap::{Channel, ChannelError};
use rustix::io::Errno;

use super::super::super::script::{Event, Opening, Script};
use super::super::{Bearer, EndReason, Links, OpenAtt, Outcome, Outputs, Timing, run_with};
use super::pods::{
    DEADLINE, EAR_PACKET, Pods, Transcript, handshake_ended, pair, spawn, spawn_dialled,
    spawn_with_fresh,
};

/// Longer than a passing test takes, so a wait of this length never runs out in one.
pub(super) const PATIENT_NS: u64 = 20_000_000_000;
/// The same wait as a script offset.
pub(super) const PATIENT_MS: u32 = 20_000;
/// The tail the tests keep the channel open for after STOP.
pub(super) const TAIL_NS: u64 = 40_000_000;
/// How a log line of an audio SDU from the pods begins after its stamp: the mark of `pods_proto::sdulog` for a packet from the pods, then the header of `pods_proto::uplink` with subtype 1.
const AUDIO_LINE: &[u8] = b" < 0400040058000100";
/// How a line of an audio notification begins after its stamp in the log of the attribute channel: the same mark, then the notification opcode, handle 0x000d and the audio marker.
const ATT_AUDIO_LINE: &[u8] = b" < 1b0d000100";
/// Ample for fake pods that wrongly stop with no channel open to have ended; ones that play on never end inside it.
const STILL_PLAYING: Duration = Duration::from_millis(200);
/// How soon the fake pods must end once nothing is left to play, well short of `DEADLINE`.
const ENDS_WITHIN: Duration = Duration::from_secs(5);

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

#[test]
fn the_fake_pods_play_on_while_an_attribute_dial_is_due_and_stop_once_the_dialler_is_gone() {
    let (dial, att_dial) = mpsc::channel();
    let played = spawn_dialled(None, att_dial, Pods::default(), Arc::default());
    let early = played.recv_timeout(STILL_PLAYING);
    assert!(
        matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
        "no channel is open, but a dial is still due: {early:?}"
    );
    drop(dial);
    let ended = played.recv_timeout(ENDS_WITHIN);
    assert!(ended.is_ok(), "the dialler is gone, so no dial can come");
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
