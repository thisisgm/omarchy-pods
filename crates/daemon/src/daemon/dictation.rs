//! The pods' attribute channel for dictation: one adapter for each link, which opens and arms the channel as the driver asks, answers what the pods ask on it, hands the audio to the tap and tells the driver the rest.

use std::fmt;
use std::future::{self, poll_fn};
use std::mem;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::thread;
use std::time::Duration;

use pods_core::policy::{DictationFact, DictationRequest};
use pods_proto::addr::BdAddr;
use pods_proto::att::{AttPdu, CONFIRMATION, REQUEST_NOT_SUPPORTED, error_response};
use pods_proto::dictation::{DictationPacket, Event};
use pods_proto::doap::{ArmStep, Arming, Handles, start, stop};
use pods_sys::l2cap::{Channel, ChannelError};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{self, JoinHandle};
use tokio::time::{Instant, sleep};
use zeroize::Zeroizing;

use super::link::{Connector, Input, Open};
use super::log;
use super::mic::Tap;

/// The policy asks at most a few things in one step, and the adapter takes one every turn.
pub(super) const DICTATION_REQUEST_QUEUE: usize = 8;

/// How long each request of an arm waits for its answer, as `cargo xtask probe` waits on the same walk.
pub(super) const ARM_ANSWER_MS: u64 = 2000;

/// The handle an error response carries when the request is refused whole (Bluetooth Core Specification 6.1, Vol 3, Part F, 3.3).
const NO_HANDLE: u16 = 0x0000;

const CONNECT_THREAD: &str = "dictation-connect";

/// The driver's end: the adapter of the link that is up, while one is.
pub(super) struct Outlet {
    connect: Connector,
    adapter: Option<(mpsc::Sender<DictationRequest>, JoinHandle<()>)>,
}

impl fmt::Debug for Outlet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut outlet = formatter.debug_struct("Outlet");
        outlet
            .field("adapter", &self.adapter)
            .finish_non_exhaustive()
    }
}

impl Outlet {
    /// `connect` dials the attribute channel of a device.
    pub(super) fn new(connect: Connector) -> Self {
        Self {
            connect,
            adapter: None,
        }
    }

    /// Starts the adapter of the link to `device`, in place of any before it; called on the runtime, when the link's control channel opens.
    pub(super) fn open(&mut self, device: BdAddr, inputs: mpsc::Sender<Input>, tap: Arc<Tap>) {
        self.close();
        let (requests, queue) = mpsc::channel(DICTATION_REQUEST_QUEUE);
        let connect = Arc::clone(&self.connect);
        let adapter = tokio::spawn(run(queue, inputs, device, connect, tap));
        self.adapter = Some((requests, adapter));
    }

    /// Ends the link's adapter and its channel with it; aborted, so it tells the driver nothing it has not queued already.
    pub(super) fn close(&mut self) {
        if let Some((_, adapter)) = self.adapter.take() {
            adapter.abort();
        }
    }

    /// Queues `request` without waiting; with no link it is dropped as a write on a closed channel is, and a queue that does not take it drops it with a log line.
    pub(super) fn offer(&self, request: DictationRequest) {
        let Some((requests, _)) = &self.adapter else {
            return;
        };
        if let Err(error) = requests.try_send(request) {
            log(&format!("dictation: request dropped: {error}"));
        }
    }
}

/// Serves one link's requests until the driver drops their sender, then drops the channel.
pub(super) async fn run(
    mut requests: mpsc::Receiver<DictationRequest>,
    inputs: mpsc::Sender<Input>,
    device: BdAddr,
    connect: Connector,
    tap: Arc<Tap>,
) {
    let mut adapter = Adapter {
        device,
        connect,
        tap,
        bearer: Bearer::Closed,
    };
    loop {
        let told = match next(&mut requests, &mut adapter.bearer).await {
            Turn::Ended => return,
            Turn::Request(request) => adapter.serve(request),
            Turn::Dialed(answer) => adapter.dialed(answer),
            Turn::Read(Some(pdu)) => adapter.packet(&pdu),
            Turn::Read(None) => adapter.over(),
            Turn::Silent => {
                log(&format!(
                    "dictation: no answer to the arm within {ARM_ANSWER_MS} ms"
                ));
                adapter.over()
            }
        };
        if let Some(fact) = told
            && inputs.send(Input::Dictation(fact)).await.is_err()
        {
            return;
        }
        // One turn, then the driver and every other task get theirs, however fast the pods talk.
        task::yield_now().await;
    }
}

enum Turn {
    Request(DictationRequest),
    /// The driver dropped its sender.
    Ended,
    /// The connect thread's answer, or none from a thread that never started.
    Dialed(Option<Result<Channel, ChannelError>>),
    /// One packet from the open channel, or `None` at its end.
    Read(Option<Zeroizing<Box<[u8]>>>),
    /// The arm's request that is out got no answer in time.
    Silent,
}

/// The arm's passed deadline, then the driver's next request, then the channel: neither of the first two waits behind pods that keep talking.
async fn next(requests: &mut mpsc::Receiver<DictationRequest>, bearer: &mut Bearer) -> Turn {
    let left = match bearer {
        Bearer::Open {
            service: Service::Arming { asked, .. },
            ..
        } => Some(Duration::from_millis(ARM_ANSWER_MS).saturating_sub(asked.elapsed())),
        Bearer::Open {
            service: Service::Armed(_),
            ..
        }
        | Bearer::Closed
        | Bearer::Dialing(_) => None,
    };
    let mut silent = pin!(async move {
        match left {
            // Served at once, as a timer rounds its end up and is not asked for a time already here.
            Some(Duration::ZERO) => {}
            Some(left) => sleep(left).await,
            None => future::pending().await,
        }
    });
    let mut channel = pin!(async {
        match bearer {
            Bearer::Closed => future::pending().await,
            // An error is a connect thread that never started, which was logged then.
            Bearer::Dialing(answer) => Turn::Dialed(answer.await.ok()),
            Bearer::Open { open, .. } => Turn::Read(open.read().await),
        }
    });
    poll_fn(|context| {
        if silent.as_mut().poll(context).is_ready() {
            return Poll::Ready(Turn::Silent);
        }
        if let Poll::Ready(request) = requests.poll_recv(context) {
            return Poll::Ready(request.map_or(Turn::Ended, Turn::Request));
        }
        channel.as_mut().poll(context)
    })
    .await
}

/// The attribute channel as its link's adapter holds it.
enum Bearer {
    Closed,
    /// The connect thread's answer is awaited.
    Dialing(oneshot::Receiver<Result<Channel, ChannelError>>),
    Open {
        open: Open,
        service: Service,
    },
}

/// How far the service on the open channel is.
enum Service {
    /// A request of the arm went out at `asked` and its answer is awaited.
    Arming {
        arming: Arming,
        asked: Instant,
    },
    Armed(Handles),
}

struct Adapter {
    device: BdAddr,
    connect: Connector,
    tap: Arc<Tap>,
    bearer: Bearer,
}

impl Adapter {
    /// A start or a stop with no armed channel is dropped: the fact that says the channel is gone is already on its way.
    fn serve(&mut self, request: DictationRequest) -> Option<DictationFact> {
        match request {
            // The driver writes the mode on the control channel and queues it nowhere.
            DictationRequest::Mode => None,
            DictationRequest::Arm => {
                self.bearer = Bearer::Dialing(self.dial());
                None
            }
            DictationRequest::Start => {
                let handles = self.armed()?;
                let lost = self.write(&start(&handles));
                if lost.is_none() {
                    self.tap.arm_dictation();
                }
                lost
            }
            DictationRequest::Stop { reason } => {
                let handles = self.armed()?;
                self.write(&stop(&handles, reason))
            }
            DictationRequest::Close => {
                self.bearer = Bearer::Closed;
                None
            }
        }
    }

    fn armed(&self) -> Option<Handles> {
        match &self.bearer {
            Bearer::Open {
                service: Service::Armed(handles),
                ..
            } => Some(*handles),
            Bearer::Open {
                service: Service::Arming { .. },
                ..
            }
            | Bearer::Closed
            | Bearer::Dialing(_) => None,
        }
    }

    /// Dials on a thread of its own, as the link does; never joined: a thread inside connect cannot be woken, and the end of the process ends it.
    fn dial(&self) -> oneshot::Receiver<Result<Channel, ChannelError>> {
        let (answer, answered) = oneshot::channel();
        let (connect, device) = (Arc::clone(&self.connect), self.device);
        let dialing = thread::Builder::new()
            .name(CONNECT_THREAD.to_owned())
            .spawn(move || {
                // An adapter that ended or dialed anew dropped its end, and the channel closes with this answer.
                answer.send(connect(device)).ok();
            });
        if let Err(error) = dialing {
            log(&format!(
                "dictation: cannot start the connect thread: {error}"
            ));
        }
        answered
    }

    /// On an open channel the arm's first request goes out; anything else is a bearer that failed.
    fn dialed(&mut self, answer: Option<Result<Channel, ChannelError>>) -> Option<DictationFact> {
        self.bearer = Bearer::Closed;
        let open = match answer {
            Some(Ok(channel)) => Open::watch(channel).map_err(|error| error.to_string()),
            Some(Err(error)) => Err(error.to_string()),
            // The connect thread never started, which was logged then.
            None => return Some(DictationFact::BearerFailed),
        };
        match open {
            Ok(open) => {
                let (arming, first) = Arming::new();
                let asked = Instant::now();
                let service = Service::Arming { arming, asked };
                self.bearer = Bearer::Open { open, service };
                self.write(&first)
            }
            Err(error) => {
                log(&format!("dictation: {error}"));
                Some(DictationFact::BearerFailed)
            }
        }
    }

    /// The pods' own requests and indications are answered whatever the channel is doing; every other packet is the arm's or the service's.
    fn packet(&mut self, pdu: &[u8]) -> Option<DictationFact> {
        match AttPdu::parse(pdu) {
            Ok(AttPdu::Request { opcode }) => {
                return self.write(&error_response(opcode, NO_HANDLE, REQUEST_NOT_SUPPORTED));
            }
            Ok(AttPdu::Indication { .. }) => return self.write(&CONFIRMATION),
            Ok(_) | Err(_) => {}
        }
        match &mut self.bearer {
            Bearer::Open {
                service: Service::Arming { arming, asked },
                ..
            } => match arming.on_pdu(pdu) {
                ArmStep::Wait => None,
                ArmStep::Send(request) => {
                    *asked = Instant::now();
                    self.write(&request)
                }
                ArmStep::Done { select, handles } => self.select(&select, handles),
                ArmStep::Failed(failure) => {
                    self.bearer = Bearer::Closed;
                    Some(DictationFact::ArmFailed(failure))
                }
            },
            Bearer::Open {
                service: Service::Armed(handles),
                ..
            } => heard(&self.tap, pdu, handles),
            Bearer::Closed | Bearer::Dialing(_) => None,
        }
    }

    /// The arm's last write, which the pods do not answer.
    fn select(&mut self, select: &[u8], handles: Handles) -> Option<DictationFact> {
        let lost = self.write(select);
        if lost.is_some() {
            return lost;
        }
        if let Bearer::Open { service, .. } = &mut self.bearer {
            *service = Service::Armed(handles);
        }
        Some(DictationFact::Armed)
    }

    /// `None` once `pdu` is written; a channel that does not take it is over.
    fn write(&mut self, pdu: &[u8]) -> Option<DictationFact> {
        let Bearer::Open { open, .. } = &self.bearer else {
            return None;
        };
        let Err(error) = open.channel().try_send(pdu) else {
            return None;
        };
        log(&format!("dictation: {error}"));
        self.over()
    }

    /// Drops the channel: an arm that was not done failed with its bearer, and an armed channel is lost, which is told this once.
    fn over(&mut self) -> Option<DictationFact> {
        match mem::replace(&mut self.bearer, Bearer::Closed) {
            Bearer::Open {
                service: Service::Armed(_),
                ..
            } => Some(DictationFact::BearerLost),
            Bearer::Open {
                service: Service::Arming { .. },
                ..
            } => Some(DictationFact::BearerFailed),
            Bearer::Closed | Bearer::Dialing(_) => None,
        }
    }
}

/// A packet of the armed service: audio goes to the tap, on this thread, and the driver hears only of the first after a start.
fn heard(tap: &Tap, pdu: &[u8], handles: &Handles) -> Option<DictationFact> {
    match DictationPacket::parse(pdu, handles) {
        Ok(Some(DictationPacket::Audio(audio))) => {
            tap.dictation(&audio).then_some(DictationFact::FirstAudio)
        }
        Ok(Some(DictationPacket::Event(event))) => Some(told(event)),
        Ok(Some(DictationPacket::Stopped { reason })) => Some(DictationFact::Refused { reason }),
        // A value of no known layout, a packet of another handle or a cut one changes nothing.
        Ok(Some(DictationPacket::Unknown { .. }) | None) | Err(_) => None,
    }
}

/// Only the score of -1 answers a start the host asked for; any other score is the pods' own business.
fn told(event: Event) -> DictationFact {
    match event {
        Event::STARTED => DictationFact::Answered,
        Event::StreamingEnd => DictationFact::Ended,
        Event::Score { .. }
        | Event::SiriActivation(_)
        | Event::SiriState(_)
        | Event::SiriCancel
        | Event::Button(_)
        | Event::Other { .. } => DictationFact::PodEvent(event),
    }
}

#[cfg(test)]
mod tests;
