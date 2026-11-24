//! What the capture command puts on the air around the attribute channel, run with a temporary home against fake pods that hold the far end of every channel it may open.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pods_proto::dictation::{SESSION_ACTIVE, STOPPED};
use pods_proto::doap::ARM_MAX_REQUESTS;
use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
use pods_proto::sdulog::{self, Direction, Record};
use pods_sys::l2cap::{Channel, ChannelError, Received};
use rustix::io::Errno;

use super::super::super::dictate::{HandleWalk, START, STOP};
use super::super::super::{CaptureArgs, capture};
use super::att_command::{interrupting_pods, soak_args};
use super::att_pods::FIND_INFORMATION;
use super::command::{Ran, STAMP, run_answered};
use super::dictation::HANDSHAKE_REQUESTS;
use super::pods::{self, DEADLINE, Pods, Transcript};
use super::{ROOT_NAME, home_with_device};

/// How long a read of a far end waits before it calls the queue empty.
const QUEUE_POLL: Duration = Duration::from_millis(2);
/// How long the fake pods hold back their reply to the stop write: inside the second the command keeps its channels open, and far past any reply the loop itself could read.
const LATE_REPLY: Duration = Duration::from_millis(300);

struct Aired {
    ran: Ran,
    pods: Transcript,
    marks: String,
    att_log: Vec<Record>,
}

impl Aired {
    /// As `1203456789\tgate\tear` is a stamp, a kind and a text.
    fn marks(&self, kind: &str) -> Vec<(u64, &str)> {
        let lines = self.marks.lines();
        lines
            .filter_map(|line| {
                let mut fields = line.splitn(3, '\t');
                let stamp = fields.next().unwrap().parse().unwrap();
                (fields.next().unwrap() == kind).then(|| (stamp, fields.next().unwrap()))
            })
            .collect()
    }

    /// The kind of every mark in order, as `1203456789\tgate\tear` holds `gate` between its stamp and its text.
    fn kinds(&self) -> Vec<&str> {
        let lines = self.marks.lines();
        lines.map(|line| line.split('\t').nth(1).unwrap()).collect()
    }
}

fn run_dir(home: &Path, run: &str) -> PathBuf {
    home.join(ROOT_NAME).join(STAMP).join(run)
}

/// Runs the capture `args` against `part`; the connect hands over the control channel, and for the attribute PSM whatever `dial_att` gives for that dial, counted from 0.
fn air(
    args: CaptureArgs,
    part: Pods,
    fars: (Channel, Option<Channel>),
    near: Channel,
    dial_att: impl Fn(usize) -> Result<Channel, ChannelError> + Send + 'static,
) -> Aired {
    air_until(args, part, fars, near, dial_att, Arc::default())
}

fn air_until(
    args: CaptureArgs,
    part: Pods,
    fars: (Channel, Option<Channel>),
    near: Channel,
    dial_att: impl Fn(usize) -> Result<Channel, ChannelError> + Send + 'static,
    interrupted: Arc<AtomicBool>,
) -> Aired {
    let home = home_with_device();
    let run = run_dir(home.path(), &args.run);
    let (far, far_att) = fars;
    let transcript = pods::spawn_on(Some(far), far_att, part, Arc::clone(&interrupted));
    let (near, dials) = (Mutex::new(Some(near)), Mutex::new(0_usize));
    let answer = move |psm| {
        if psm == CONTROL_PSM {
            let near = near.lock().unwrap().take();
            return near.ok_or(ChannelError::Connect(Errno::CONNREFUSED));
        }
        let mut dials = dials.lock().unwrap();
        let dial = *dials;
        *dials = dial.saturating_add(1);
        dial_att(dial)
    };
    let ran = run_answered(home.path(), answer, move |host| {
        capture(&args, host, &interrupted)
    });
    let pods = transcript.recv_timeout(DEADLINE);
    let att_log = fs::read_to_string(run.join("att.log")).unwrap();
    let records = att_log.lines();
    Aired {
        ran,
        pods: pods.expect("the fake pods did not finish before the deadline"),
        marks: fs::read_to_string(run.join("marks.tsv")).unwrap(),
        att_log: records
            .map(|line| sdulog::parse_line(line).unwrap())
            .collect(),
    }
}

/// A connect that hands over each of `lines` once, in order, and refuses every dial after them.
fn in_turn(lines: Vec<Channel>) -> impl Fn(usize) -> Result<Channel, ChannelError> + Send {
    let lines = Mutex::new(lines.into_iter());
    move |_dial| {
        let line = lines.lock().unwrap().next();
        line.ok_or(ChannelError::Connect(Errno::CONNREFUSED))
    }
}

#[test]
fn the_attribute_channel_is_dialled_only_once_the_handshake_is_done() {
    let part = interrupting_pods();
    let handshaken = Arc::clone(&part.handshaken);
    let ((near, far), (near_att, far_att)) = (pods::pair(), pods::pair());
    let near_att = Mutex::new(Some(near_att));
    // The pods refuse a connect that comes while the handshake has not ended, as one sent ahead of it would.
    let dial_att = move |_dial| {
        if !pods::handshake_ended(&handshaken) {
            return Err(ChannelError::Connect(Errno::CONNREFUSED));
        }
        let near_att = near_att.lock().unwrap().take();
        near_att.ok_or(ChannelError::Connect(Errno::CONNREFUSED))
    };
    let args = soak_args("order", false);
    let aired = air(args, part, (far, Some(far_att)), near, dial_att);

    assert_eq!(aired.ran.dialled, [CONTROL_PSM, ATT_PSM]);
    let said = "xtask probe: capture ended early: interrupted by a signal; the dictation stop was sent (packets in ";
    assert!(aired.ran.stderr.starts_with(said), "{}", aired.ran.stderr);
    let kinds = aired.kinds();
    let ready = kinds.iter().position(|kind| *kind == "ready");
    let connect = kinds.iter().position(|kind| *kind == "att-connect");
    assert!(ready.is_some() && ready < connect, "{kinds:?}");
    let texts: Vec<&str> = aired
        .marks("att-connect")
        .into_iter()
        .map(|mark| mark.1)
        .collect();
    assert_eq!(texts, ["connecting", "attribute channel open"]);
}

#[test]
fn a_signal_during_the_attribute_connect_ends_the_run_before_anything_is_sent_on_it() {
    let ((near, far), (near_att, far_att)) = (pods::pair(), pods::pair());
    let interrupted = Arc::new(AtomicBool::new(false));
    let (raised, near_att) = (Arc::clone(&interrupted), Mutex::new(Some(near_att)));
    let dial_att = move |_dial| {
        raised.store(true, Ordering::SeqCst);
        let near_att = near_att.lock().unwrap().take();
        near_att.ok_or(ChannelError::Connect(Errno::CONNREFUSED))
    };
    let args = soak_args("signal", false);
    let fars = (far, Some(far_att));
    let aired = air_until(args, Pods::default(), fars, near, dial_att, interrupted);

    assert_eq!(aired.pods.att_got, Vec::<Vec<u8>>::new());
    assert_eq!(aired.pods.got(), HANDSHAKE_REQUESTS);
    let said = "xtask probe: capture ended early: interrupted by a signal (packets in ";
    assert!(aired.ran.stderr.starts_with(said), "{}", aired.ran.stderr);
}

/// Every packet in `far`'s queue, and whether the other end had been closed behind them.
fn left_on(far: &Channel) -> (Vec<Vec<u8>>, bool) {
    far.set_recv_timeout(QUEUE_POLL).unwrap();
    let mut buf = vec![0_u8; 64];
    let mut packets = Vec::new();
    loop {
        match far.recv(&mut buf).unwrap() {
            Received::Sdu(len) => packets.push(buf[..len].to_vec()),
            Received::Closed => return (packets, true),
            Received::TimedOut | Received::Truncated { .. } => return (packets, false),
        }
    }
}

#[test]
fn a_walk_that_gets_no_answer_closes_its_channel_and_unchecked_starts_on_a_fresh_one() {
    let ((near, far), (near_first, far_first), (near_fresh, far_fresh)) =
        (pods::pair(), pods::pair(), pods::pair());
    // Nobody plays the first attribute channel, so its one request gets no answer; the second dial reads what the first was left with.
    let left = Arc::new(Mutex::new(None));
    let (seen, lines) = (
        Arc::clone(&left),
        Mutex::new([Some(near_first), Some(near_fresh)]),
    );
    let dial_att = move |dial: usize| {
        if dial == 1 {
            *seen.lock().unwrap() = Some(left_on(&far_first));
        }
        let line = lines.lock().unwrap().get_mut(dial).and_then(Option::take);
        line.ok_or(ChannelError::Connect(Errno::CONNREFUSED))
    };
    let args = CaptureArgs {
        unchecked: true,
        ..soak_args("fresh", false)
    };
    let aired = air(
        args,
        interrupting_pods(),
        (far, Some(far_fresh)),
        near,
        dial_att,
    );

    assert_eq!(aired.ran.dialled, [CONTROL_PSM, ATT_PSM, ATT_PSM]);
    // The first channel carried the one request and was closed before the second was asked for.
    let first = (vec![HandleWalk::first().to_vec()], true);
    assert_eq!(*left.lock().unwrap(), Some(first));
    // The fresh channel carried no request at all: the start write, then its stop.
    assert_eq!(aired.pods.att_got, [&START[..], &STOP]);
    let said = "xtask probe: capture ended early: interrupted by a signal; the dictation stop was sent (packets in ";
    assert!(aired.ran.stderr.starts_with(said), "{}", aired.ran.stderr);
    let closed = aired.marks("att-close");
    let texts: Vec<&str> = closed.iter().map(|mark| mark.1).collect();
    assert_eq!(texts, ["a request of the walk got no answer in 2000 ms"]);
    let connects = aired.marks("att-connect");
    assert_eq!(connects.len(), 4, "{}", aired.marks);
    assert!(closed[0].0 <= connects[2].0, "{}", aired.marks);
    let handles: Vec<&str> = aired
        .marks("handles")
        .into_iter()
        .map(|mark| mark.1)
        .collect();
    assert_eq!(handles, ["unchecked"]);
}

#[test]
fn a_walk_that_gets_no_answer_closes_its_channel_and_sends_nothing_more_unless_waived() {
    let ((near, far), (near_first, far_first)) = (pods::pair(), pods::pair());
    let args = soak_args("silent", false);
    let dial_att = in_turn(vec![near_first]);
    let aired = air(args, interrupting_pods(), (far, None), near, dial_att);

    assert_eq!(aired.ran.dialled, [CONTROL_PSM, ATT_PSM]);
    assert_eq!(
        left_on(&far_first),
        (vec![HandleWalk::first().to_vec()], true)
    );
    assert_eq!(aired.pods.got(), HANDSHAKE_REQUESTS);
    assert_eq!(aired.ran.code, format!("{:?}", ExitCode::FAILURE));
    let said = "xtask probe: capture ended early: the pods listed no attribute, so the start write was not sent (packets in ";
    assert!(aired.ran.stderr.starts_with(said), "{}", aired.ran.stderr);
    let texts: Vec<&str> = aired
        .marks("att-close")
        .into_iter()
        .map(|mark| mark.1)
        .collect();
    assert_eq!(texts, ["a request of the walk got no answer in 2000 ms"]);
    let handles: Vec<&str> = aired
        .marks("handles")
        .into_iter()
        .map(|mark| mark.1)
        .collect();
    assert_eq!(handles, ["no listing"]);
}

#[test]
fn a_reply_to_the_stop_that_comes_in_the_tail_is_logged_and_marked() {
    let mut part = interrupting_pods();
    part.att.stopped_after = LATE_REPLY;
    let ((near, far), (near_att, far_att)) = (pods::pair(), pods::pair());
    let args = soak_args("tail", false);
    let aired = air(
        args,
        part,
        (far, Some(far_att)),
        near,
        in_turn(vec![near_att]),
    );

    let said = "xtask probe: capture ended early: interrupted by a signal; the dictation stop was sent (packets in ";
    assert!(aired.ran.stderr.starts_with(said), "{}", aired.ran.stderr);
    let stop_ns = aired.marks("dictation-stop")[0].0;
    let late_ns = u64::try_from(LATE_REPLY.as_nanos()).unwrap();
    let replies: Vec<&Record> = aired
        .att_log
        .iter()
        .filter(|record| record.direction == Direction::FromPods && record.sdu == STOPPED)
        .collect();
    assert_eq!(replies.len(), 1, "{:?}", aired.att_log);
    assert!(replies[0].at_ns >= stop_ns + late_ns, "{replies:?}");
    let stopped = aired.marks("stopped");
    assert_eq!(stopped.len(), 1, "{}", aired.marks);
    assert_eq!(stopped[0].0, replies[0].at_ns);
    let delay_ns = replies[0].at_ns - stop_ns;
    assert_eq!(stopped[0].1, format!("{delay_ns} ns after the stop write"));
    // The reply is in the summary's count too, and the end is marked after the tail.
    assert_eq!(aired.kinds().last(), Some(&"end"));
    let counted = format!("attribute packets in {}, ", aired.pods.att_sent.len());
    assert!(aired.ran.stderr.contains(&counted), "{}", aired.ran.stderr);
}

/// Pods whose table holds `attributes` handles from 1 on and who list one a reply, so a walk of it sends one request more than it holds.
fn single_file_pods(attributes: u32) -> Pods {
    let mut part = interrupting_pods();
    part.att.table = Some((1..=u16::try_from(attributes).unwrap()).collect());
    part.att.page = 1;
    part
}

#[test]
fn a_table_that_takes_every_request_the_walk_may_send_is_walked_and_one_longer_is_cut() {
    let most = usize::try_from(ARM_MAX_REQUESTS).unwrap();
    let ((near, far), (near_att, far_att)) = (pods::pair(), pods::pair());
    let part = single_file_pods(ARM_MAX_REQUESTS - 1);
    let args = soak_args("under", false);
    let aired = air(
        args,
        part,
        (far, Some(far_att)),
        near,
        in_turn(vec![near_att]),
    );

    let (walk, stream) = aired.pods.att_got.split_at(most);
    assert!(
        walk.iter().all(|pdu| pdu[0] == FIND_INFORMATION),
        "{walk:02x?}"
    );
    assert_eq!(stream, [&START[..], &STOP]);
    let handles: Vec<&str> = aired
        .marks("handles")
        .into_iter()
        .map(|mark| mark.1)
        .collect();
    assert_eq!(handles, ["present"]);
    assert_eq!(aired.marks("att-close"), []);

    let ((near, far), (near_att, far_att)) = (pods::pair(), pods::pair());
    let part = single_file_pods(ARM_MAX_REQUESTS);
    let args = soak_args("over", false);
    let aired = air(
        args,
        part,
        (far, Some(far_att)),
        near,
        in_turn(vec![near_att]),
    );

    let walk = &aired.pods.att_got;
    assert_eq!(walk.len(), most, "{walk:02x?}");
    assert!(
        walk.iter().all(|pdu| pdu[0] == FIND_INFORMATION),
        "{walk:02x?}"
    );
    assert_eq!(aired.pods.got(), HANDSHAKE_REQUESTS);
    assert!(!aired.pods.got().contains(&&SESSION_ACTIVE[..]));
    assert_eq!(aired.ran.code, format!("{:?}", ExitCode::FAILURE));
    let said = "xtask probe: capture ended early: the walk of the pods' attribute table was cut before its end, so the start write was not sent (packets in ";
    assert!(aired.ran.stderr.starts_with(said), "{}", aired.ran.stderr);
    let texts: Vec<&str> = aired
        .marks("att-close")
        .into_iter()
        .map(|mark| mark.1)
        .collect();
    assert_eq!(texts, ["the table had not ended after 128 requests"]);
    let handles: Vec<&str> = aired
        .marks("handles")
        .into_iter()
        .map(|mark| mark.1)
        .collect();
    assert_eq!(handles, ["present"]);
}
