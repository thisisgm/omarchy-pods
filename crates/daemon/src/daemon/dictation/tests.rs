//! Support tests: the adapter in process, against the fake pods and a pods' end the test holds itself.

use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::thread;
use std::time::Duration;

use pods_core::policy::{DictationFact, DictationRequest};
use pods_fake::doap::{Doap, MODE_SETTLE, Table};
use pods_fake::{Pods, Profile, pair};
use pods_proto::addr::BdAddr;
use pods_proto::dictation::{DictationPacket, Event};
use pods_proto::doap::{
    ArmFailure, Handles, STOP_REASON_HOST, STOP_REASON_UNSOLICITED, doap_mode_on,
};
use pods_samples::doap::{self as sample, host};
use pods_samples::opus;
use pods_sys::l2cap::{Channel, ChannelError, Received};
use rustix::io::Errno;
use tokio::runtime::Builder;
use tokio::sync::mpsc::{self, error::TryRecvError};
use tokio::task::{self, JoinHandle};
use tokio::time::{self, sleep, timeout};

use super::{ARM_ANSWER_MS, ArmId, DICTATION_REQUEST_QUEUE, Outlet, TaggedRequest, run, told};
use crate::daemon::link::tests::{HANG_LIMIT, refusing};
use crate::daemon::link::{self, Connector, Input, Open};
use crate::daemon::mic::Tap;

mod armed;
mod arming;
mod flood;

const POLL: Duration = Duration::from_millis(5);
/// The audio notifications a test lets through before it looks at what the driver was told.
const HEARD: u64 = 10;
/// The opcode of the MTU request, which stands for any request of the pods' own.
const MTU_REQUEST: u8 = 0x02;
/// An ATT error answer takes five bytes, so eight bytes leave room to receive it whole.
const ANSWER_BUFFER_BYTES: usize = 8;
/// A notification of a handle no table holds, which answers nothing.
const UNASKED_NOTIFICATION: [u8; 3] = [0x1b, 0x40, 0x00];
/// Fewer notifications than this in a full socket would make a flood no test of anything.
const FLOOD_MIN: usize = 32;
/// How many times over a test refills the socket before it calls a flood endless.
const REFILLS: usize = 16;
/// The parts of the answer time a test lets pass before and after it looks.
const MOST_OF_IT: Duration = Duration::from_millis(1500);
const THE_REST: Duration = Duration::from_millis(ARM_ANSWER_MS)
    .checked_sub(MOST_OF_IT)
    .expect("MOST_OF_IT exceeds the arm answer time");
const ONE_MS: Duration = Duration::from_millis(1);
/// A timer ends on the next whole millisecond, so a held clock is moved one past the end it must reach.
const PAST_THE_END: Duration = Duration::from_millis(2);
/// The turns a test gives the adapter for what needs no time to pass; a held clock makes a timeout no bound.
const TURNS: usize = 1024;
/// A score the pods give a start nobody here asked for.
const OTHER_SCORE: Event = Event::Score {
    bits: 0.5_f32.to_bits(),
};

fn device() -> BdAddr {
    "02:00:00:00:00:01".parse().unwrap()
}

/// A current thread runtime with a clock and a poller, as the daemon's own.
fn on_runtime<F: Future>(test: F) -> F::Output {
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(test)
}

/// While alive, the paused clock moves only when the test advances it: a waiting blocking task keeps the runtime from jumping to the next timer.
struct HeldClock {
    _release: std_mpsc::Sender<()>,
}

/// The hold lets go after `HANG_LIMIT` of real time, so a test that hangs on the held clock runs into its own bounds and fails.
fn hold_clock() -> HeldClock {
    time::pause();
    let (release, held) = std_mpsc::channel::<()>();
    drop(task::spawn_blocking(move || held.recv_timeout(HANG_LIMIT)));
    HeldClock { _release: release }
}

/// The adapter on the test's runtime, with the two queues its driver holds.
struct Bench {
    requests: mpsc::Sender<TaggedRequest>,
    facts: mpsc::Receiver<Input>,
    tap: Arc<Tap>,
    adapter: JoinHandle<()>,
}

impl Bench {
    fn start(connect: Connector) -> Self {
        let (requests, queue) = mpsc::channel(DICTATION_REQUEST_QUEUE);
        let (inputs, facts) = link::queue();
        let tap = Tap::new();
        let adapter = tokio::spawn(run(queue, inputs, device(), connect, Arc::clone(&tap)));
        Self {
            requests,
            facts,
            tap,
            adapter,
        }
    }

    async fn ask(&self, request: DictationRequest) {
        self.requests
            .send((ArmId::default(), request))
            .await
            .unwrap();
    }

    /// The next thing the driver is told, which must come inside `HANG_LIMIT`.
    async fn told(&mut self) -> DictationFact {
        match timeout(HANG_LIMIT, self.facts.recv()).await {
            Ok(Some(Input::Dictation(_, fact))) => fact,
            other => panic!("a dictation fact within {HANG_LIMIT:?}, not {other:?}"),
        }
    }

    /// The next thing the driver is told, which must come with the clock where it is.
    async fn told_now(&mut self) -> DictationFact {
        for _ in 0..TURNS {
            if let Some(fact) = self.told_so_far() {
                return fact;
            }
            task::yield_now().await;
        }
        panic!("no dictation fact within {TURNS} turns");
    }

    /// What the driver was told and has not read yet; none when it was told nothing.
    fn told_so_far(&mut self) -> Option<DictationFact> {
        match self.facts.try_recv() {
            Ok(Input::Dictation(_, fact)) => Some(fact),
            Ok(other) => panic!("a dictation fact, not {other:?}"),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => panic!("the dictation adapter died"),
        }
    }

    async fn arm(&mut self) {
        self.ask(DictationRequest::Arm).await;
        assert_eq!(self.told().await, DictationFact::Armed);
    }
}

/// The fake pods with a control channel of this connection, which the mode is set on; the drop ends every serving thread and waits for it.
struct Served {
    pods: Pods,
    doap: Doap,
    control: Channel,
}

impl Served {
    fn new() -> Self {
        let pods = Pods::new(Profile::default());
        let (control, end) = pair();
        pods.serve(end);
        Self {
            doap: pods.doap(),
            pods,
            control,
        }
    }

    /// A connector that hands every dial an attribute channel of these pods.
    fn connector(&self) -> Connector {
        let pods = self.pods.clone();
        Arc::new(move |_| {
            let (host, end) = pair();
            pods.serve_att(end);
            Ok(host)
        })
    }

    /// The mode as the driver writes it; returns once the fake accepts START.
    async fn mode(&self) {
        self.control.try_send(&doap_mode_on()).unwrap();
        wait_for("the mode heard", || self.doap.mode_seen()).await;
        sleep(MODE_SETTLE).await;
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        let finished = self.pods.clone().finish();
        if !thread::panicking() {
            finished.unwrap();
        }
    }
}

/// The pods' end of one attribute channel in the test's own hands, for what the fake pods never do.
struct Peer {
    open: Open,
}

impl Peer {
    /// A connector whose first dial gets the host's end, and the pods' end; a later dial is refused.
    fn connector() -> (Connector, Self) {
        let (host, end) = pair();
        let host = Mutex::new(Some(host));
        let connect: Connector = Arc::new(move |_| {
            let host = host.lock().unwrap().take();
            host.ok_or(ChannelError::Connect(Errno::CONNREFUSED))
        });
        let open = Open::watch(end).unwrap();
        (connect, Self { open })
    }

    /// The next packet the host wrote, or none once it closed the channel.
    async fn read(&mut self) -> Option<Vec<u8>> {
        let read = timeout(HANG_LIMIT, self.open.read()).await;
        let pdu = read.unwrap_or_else(|_| panic!("nothing from the host within {HANG_LIMIT:?}"));
        pdu.map(|pdu| pdu.to_vec())
    }

    fn send(&self, pdu: &[u8]) {
        self.open.channel().try_send(pdu).unwrap();
    }

    /// Fills the socket with notifications that answer nothing, and counts them.
    fn flood(&self) -> usize {
        let mut sent = 0_usize;
        while self.open.channel().try_send(&UNASKED_NOTIFICATION).is_ok() {
            sent = sent.saturating_add(1);
        }
        sent
    }

    /// A full socket the host has begun to read, so it is in the flood when this returns; the count is of the first fill.
    async fn flooded(&self) -> usize {
        let flood = self.flood();
        for _ in 0..TURNS {
            if self.flood() > 0 {
                return flood;
            }
            task::yield_now().await;
        }
        panic!("the host read nothing of the flood within {TURNS} turns");
    }

    /// The host closed its end, with the clock where it is, so no timer of the adapter is what closed it.
    async fn closed_now(&self) {
        let mut unread = [0_u8; ANSWER_BUFFER_BYTES];
        for _ in 0..TURNS {
            match self.open.channel().try_recv(&mut unread) {
                // The socket reports a reset when the host closes with unread packets.
                Ok(Received::Closed) | Err(ChannelError::Recv(Errno::CONNRESET)) => return,
                Ok(Received::Sdu(_) | Received::Truncated { .. } | Received::TimedOut) => {}
                Err(error) => panic!("checking for the host's close failed: {error}"),
            }
            task::yield_now().await;
        }
        panic!("the host kept its end for {TURNS} turns");
    }

    /// A request of the pods' own and the host's refusal of it, which shows every packet before it was taken.
    async fn barrier(&mut self) {
        self.send(&sample::request(MTU_REQUEST));
        assert_eq!(self.read().await.unwrap(), host::refusal(MTU_REQUEST));
    }
}

async fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let waited = timeout(HANG_LIMIT, async {
        while !ready() {
            sleep(POLL).await;
        }
    });
    waited
        .await
        .unwrap_or_else(|_| panic!("{what} not seen within {HANG_LIMIT:?}"));
}

/// A dictation notification the tap has not seen, to ask it whether a start still waits for its audio.
fn tap_waits_for_audio(tap: &Tap) -> bool {
    let frame = opus::tone().next().unwrap();
    let pdu = sample::audio(&sample::Handles::OBSERVED, 0, &[(0, frame)]);
    let Ok(Some(DictationPacket::Audio(audio))) = DictationPacket::parse(&pdu, &Handles::OBSERVED)
    else {
        panic!("not a dictation audio notification");
    };
    tap.dictation(&audio) == Some(true)
}

const TABLE: sample::Handles = sample::Handles::OBSERVED;

/// Answers one whole arm as the observed table does, from the first request on.
async fn arm_by_hand(peer: &mut Peer) {
    assert_eq!(peer.read().await.unwrap(), host::declarations_from(1));
    peer.send(&sample::declarations(&TABLE.declared()));
    let [.., last] = TABLE.declared();
    let after = last.at.checked_add(1).unwrap();
    assert_eq!(peer.read().await.unwrap(), host::declarations_from(after));
    let no_more = sample::error(
        sample::READ_BY_TYPE_REQUEST,
        after,
        sample::ATTRIBUTE_NOT_FOUND,
    );
    peer.send(&no_more);
    let pass = peer.read().await.unwrap();
    assert_eq!(pass.first(), Some(&sample::FIND_INFORMATION_REQUEST));
    let descriptors = TABLE.cccds.map(|at| (at, sample::CONFIGURATION));
    peer.send(&sample::short_types(&descriptors));
    for descriptor in TABLE.cccds {
        assert_eq!(peer.read().await.unwrap(), host::subscribe(descriptor));
        peer.send(&sample::WRITTEN);
    }
    assert_eq!(
        peer.read().await.unwrap(),
        host::read(TABLE.supported_codecs)
    );
    peer.send(&sample::CODECS);
    assert_eq!(peer.read().await.unwrap(), host::select_codec(&TABLE));
}
