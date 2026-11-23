//! The pods' microphone as a source node on the desktop's audio server: opened and closed as the driver asks, what becomes of it told back, and tried again on the audio ladder while it is wanted.

mod tap;

use std::future::{self, poll_fn};
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::task::Poll;
use std::time::Duration;

use pods_core::policy::{MicFact, MicRequest, OpenId};
use pods_mic::{NODE_LATENCY_MS, RATE_HZ, Resync};
use pods_pw::{Event, OpenError, Source, Spec};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, spawn_blocking};
use tokio::time::{Instant, sleep_until, timeout};

use super::audio::AUDIO_RETRY_MS;
use super::link::{Input, STOP_WAIT_MS};
use super::log;
pub(super) use tap::Tap;

/// The node's `priority.session`, which the design sets.
pub(super) const NODE_PRIORITY: u32 = 3100;
/// Events are edges; sixteen is a restart storm.
pub(super) const MIC_EVENT_QUEUE: usize = 16;

/// Where the audio server listens, under the runtime directory.
const SOCKET: &str = "pipewire-0";
const NANOS_PER_MILLI: u64 = 1_000_000;

/// The latest event of a source, held in one atomic.
const PAUSED: u8 = 0;
const STREAMING: u8 = 1;
const LOST: u8 = 2;

const ADAPTER_GONE: &str = "mic: the adapter has ended; request dropped";
/// Logged once an outage, and before every library failure, with the reason after it.
const OPEN_FAILED: &str = "mic: source could not open: ";
const REFUSED_CONFIG: &str = "the decoder refused the uplink's configuration";

/// Replaces the request the adapter has not read yet, so the latest is the one served; an ended adapter drops it with a log line.
pub(super) fn offer(requests: &watch::Sender<MicRequest>, request: MicRequest) {
    if requests.send(request).is_err() {
        log(ADAPTER_GONE);
    }
}

/// Serves the requests until the driver drops their sender, then takes the node off the server.
pub(super) async fn run(
    mut requests: watch::Receiver<MicRequest>,
    inputs: mpsc::Sender<Input>,
    runtime_dir: PathBuf,
    tap: Arc<Tap>,
) {
    let (wake, mut wakes) = mpsc::channel(MIC_EVENT_QUEUE);
    let mut adapter = Adapter {
        socket: runtime_dir.join(SOCKET),
        tap,
        wake,
        wanted: None,
        opened: None,
        misses: 0,
        retry_at: None,
    };
    loop {
        let fact = match next(&mut requests, &mut wakes, adapter.retry_at).await {
            Turn::Request => {
                let request = requests.borrow_and_update().clone();
                adapter.serve(request).await
            }
            Turn::Ended => break,
            Turn::Woken => adapter.woken().await,
            Turn::Retry => adapter.open().await,
        };
        if let Some(fact) = fact
            && inputs.send(Input::Mic(fact)).await.is_err()
        {
            break;
        }
    }
    adapter.close().await;
}

/// Ends the adapter inside `STOP_WAIT_MS` once the driver is gone, so the node leaves the server before the daemon exits.
pub(super) async fn stop(mut adapter: JoinHandle<()>) {
    match timeout(Duration::from_millis(STOP_WAIT_MS), &mut adapter).await {
        Ok(Ok(())) => {}
        Ok(Err(ended)) => log(&format!("mic: the adapter ended abnormally: {ended}")),
        Err(_) => {
            log(&format!(
                "mic: the adapter did not stop within {STOP_WAIT_MS} ms; aborting it"
            ));
            adapter.abort();
            // Awaited so the queue is dropped first; the result is only the cancellation.
            drop(adapter.await);
        }
    }
}

enum Turn {
    /// The latest request is unread.
    Request,
    /// The driver dropped its sender.
    Ended,
    Woken,
    Retry,
}

/// The next request, wake or retry, the driver's requests first.
async fn next(
    requests: &mut watch::Receiver<MicRequest>,
    wakes: &mut mpsc::Receiver<()>,
    retry_at: Option<Instant>,
) -> Turn {
    let mut retry = pin!(async move {
        match retry_at {
            Some(at) => sleep_until(at).await,
            None => future::pending().await,
        }
    });
    let mut changed = pin!(requests.changed());
    poll_fn(|context| {
        if let Poll::Ready(changed) = changed.as_mut().poll(context) {
            return Poll::Ready(changed.map_or(Turn::Ended, |()| Turn::Request));
        }
        // The adapter holds a sender of its own wakes, so that queue never closes.
        if let Poll::Ready(Some(())) = wakes.poll_recv(context) {
            return Poll::Ready(Turn::Woken);
        }
        retry.as_mut().poll(context).map(|()| Turn::Retry)
    })
    .await
}

struct Opened {
    id: OpenId,
    source: Source,
    /// Written on the library's loop thread.
    events: Arc<AtomicU8>,
    /// What the driver was last told.
    recording: bool,
}

struct Adapter {
    socket: PathBuf,
    tap: Arc<Tap>,
    wake: mpsc::Sender<()>,
    /// The open last asked for, with its node's name and description, until a close.
    wanted: Option<(OpenId, String, String)>,
    opened: Option<Opened>,
    /// Failed tries since the node was last up or asked for, which pick the ladder's step.
    misses: usize,
    retry_at: Option<Instant>,
}

impl Adapter {
    /// An open replaces whatever source the adapter holds; a close ends the source and its retries.
    async fn serve(&mut self, request: MicRequest) -> Option<MicFact> {
        self.close().await;
        match request {
            MicRequest::Open {
                id,
                name,
                description,
            } => {
                self.wanted = Some((id, name, description));
                self.misses = 0;
                self.open().await
            }
            MicRequest::Close => {
                self.wanted = None;
                self.retry_at = None;
                None
            }
        }
    }

    /// Tries the node asked for; the library blocks, so the try runs off the runtime.
    async fn open(&mut self) -> Option<MicFact> {
        self.retry_at = None;
        let (id, name, description) = self.wanted.clone()?;
        let Some((chain, feed, resync)) = self.tap.chain() else {
            return Some(self.broken(&REFUSED_CONFIG));
        };
        let events = Arc::new(AtomicU8::new(PAUSED));
        let watch = watcher(Arc::clone(&events), self.wake.clone(), resync);
        let socket = self.socket.clone();
        let tap = Arc::clone(&self.tap);
        let opened = spawn_blocking(move || {
            let spec = Spec {
                remote: &socket,
                name: &name,
                description: &description,
                rate: RATE_HZ,
                latency_ns: u64::from(NODE_LATENCY_MS).saturating_mul(NANOS_PER_MILLI),
                priority: NODE_PRIORITY,
            };
            tap.open(chain, || Source::open(&spec, Box::new(feed), watch))
        })
        .await;
        match opened {
            Ok(Ok(source)) => {
                self.misses = 0;
                self.opened = Some(Opened {
                    id,
                    source,
                    events,
                    recording: false,
                });
                Some(MicFact::Up(id))
            }
            Ok(Err(error @ (OpenError::NoServer(_) | OpenError::Refused(_)))) => {
                if self.misses == 0 {
                    log(&format!("{OPEN_FAILED}{error}"));
                }
                Some(self.retry(id))
            }
            Ok(Err(error @ OpenError::Library { .. })) => Some(self.broken(&error)),
            // A panic inside the binding is the library failing too.
            Err(panicked) => Some(self.broken(&panicked)),
        }
    }

    fn broken(&mut self, error: &dyn std::fmt::Display) -> MicFact {
        log(&format!("{OPEN_FAILED}{error}"));
        self.wanted = None;
        MicFact::Broken
    }

    /// `Down`, with the next try one step of the ladder away, staying on its last step.
    fn retry(&mut self, id: OpenId) -> MicFact {
        let [.., longest] = AUDIO_RETRY_MS;
        let wait = AUDIO_RETRY_MS.get(self.misses).copied().unwrap_or(longest);
        self.misses = self.misses.saturating_add(1);
        self.retry_at = Instant::now().checked_add(Duration::from_millis(wait));
        MicFact::Down(id)
    }

    /// What the latest event changed; a stale wake, or one for a source already closed, changes nothing.
    async fn woken(&mut self) -> Option<MicFact> {
        let opened = self.opened.as_ref()?;
        let (id, event) = (opened.id, opened.events.load(Ordering::Acquire));
        if event == LOST {
            self.close().await;
            return Some(self.retry(id));
        }
        let opened = self.opened.as_mut()?;
        let recording = event == STREAMING;
        if recording == opened.recording {
            return None;
        }
        opened.recording = recording;
        Some(MicFact::Recording(id, recording))
    }

    /// The drop takes the loop's lock and joins its thread, so it runs off the runtime.
    async fn close(&mut self) {
        let Some(opened) = self.opened.take() else {
            return;
        };
        let tap = Arc::clone(&self.tap);
        let closed = spawn_blocking(move || tap.close(opened.source));
        if let Err(error) = closed.await {
            log(&format!("mic: the source's drop ended abnormally: {error}"));
        }
    }
}

/// Runs on the library's loop thread: a recorder that starts the stream restarts the reader at its target, the latest event goes in `events`, where a lost source stays lost, then a wake that may be dropped.
fn watcher(
    events: Arc<AtomicU8>,
    wake: mpsc::Sender<()>,
    resync: Resync,
) -> Box<dyn FnMut(Event) + Send> {
    Box::new(move |event| {
        if event == Event::Streaming {
            resync.request();
        }
        let word = match event {
            Event::Streaming => STREAMING,
            Event::Paused => PAUSED,
            Event::Lost => LOST,
        };
        if events.load(Ordering::Acquire) != LOST {
            events.store(word, Ordering::Release);
        }
        // A full queue already holds a wake, and the atomic holds the truth.
        wake.try_send(()).ok();
    })
}

#[cfg(test)]
mod tests;
