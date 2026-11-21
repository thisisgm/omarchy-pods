//! One protocol 1 client: requests answered in order, and after `subscribe` one event line per state change.

mod reply;
mod request;

use std::collections::VecDeque;
use std::future::poll_fn;
use std::io::{self, ErrorKind};
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use pods_core::state::State;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task;
use tokio::time::{Sleep, sleep};

use super::hub::Hub;
use super::log;
use request::Request;

/// The longest request line, newline excluded; a longer one closes the connection.
const MAX_LINE_BYTES: usize = 4096;

/// What `pending` may hold: one line at the bound plus the byte that shows it is past it.
const PENDING_BYTES: usize = MAX_LINE_BYTES + 1;

/// Lines waiting for a slow client; an event that finds the queue full closes the connection instead of being skipped.
const QUEUE_LINES: usize = 16;

/// How long a client may leave a line unfinished, a reply unread or, unsubscribed, the connection idle; twice the legacy request deadline.
const STALL_DEADLINE: Duration = Duration::from_millis(2000);

/// What woke the connection.
#[derive(Debug)]
enum Wake {
    /// Bytes from the client, now at the end of `pending`; zero is the end of its stream.
    Received(io::Result<usize>),
    /// A state change, or `None` when the hub dropped this subscriber for falling behind.
    Changed(Option<State>),
    /// Bytes of the first queued line taken by the socket.
    Wrote(io::Result<usize>),
    /// The client made no progress for `STALL_DEADLINE` while the connection waited on it.
    Stalled,
}

/// The state of one connection between wakes.
#[derive(Debug)]
struct Connection {
    stream: UnixStream,
    /// Received bytes not yet answered, at most `PENDING_BYTES`.
    pending: Vec<u8>,
    chunk: [u8; PENDING_BYTES],
    /// Replies and events not yet written, at most `QUEUE_LINES`.
    queue: VecDeque<String>,
    /// How much of the first queued line the socket has taken.
    written: usize,
    /// Changes after the last `subscribe`, if the client subscribed.
    changes: Option<mpsc::Receiver<State>>,
    /// Whether the client ended its side, after which no more requests are read and the connection closes once the queue is written.
    ended: bool,
    /// Runs while the connection waits on the client and restarts at each answered request and each written line.
    stall: Option<Pin<Box<Sleep>>>,
}

/// Serves one client whose first bytes, already read, are `received`; returns when either side is done.
pub(super) async fn serve_connection(stream: UnixStream, hub: Hub, received: Vec<u8>) {
    let mut connection = Connection {
        stream,
        pending: received,
        chunk: [0; PENDING_BYTES],
        queue: VecDeque::new(),
        written: 0,
        changes: None,
        ended: false,
        stall: None,
    };
    if let Err(reason) = connection.run(&hub).await {
        log(&format!(
            "control socket: protocol 1 client closed: {reason}"
        ));
    }
}

impl Connection {
    /// Answers, reads, writes and relays changes until the client is gone or must be closed.
    async fn run(&mut self, hub: &Hub) -> Result<(), String> {
        loop {
            self.answer(hub).await?;
            if self.owes_nothing_after_its_end() {
                return Ok(());
            }
            self.watch_for_stall();
            match poll_fn(|context| self.poll_wake(context)).await {
                Wake::Received(Ok(0)) => self.ended = true,
                Wake::Received(Ok(read)) => self
                    .pending
                    .extend_from_slice(self.chunk.get(..read).unwrap_or_default()),
                Wake::Received(Err(error)) => {
                    return Err(format!("cannot read a request: {error}"));
                }
                Wake::Changed(Some(state)) => self.enqueue_event(&state)?,
                Wake::Changed(None) => {
                    return Err(
                        "the hub dropped it for falling behind the state changes".to_owned()
                    );
                }
                Wake::Wrote(Ok(sent)) => self.advance(sent)?,
                // A client that hangs up without reading every reply is normal.
                Wake::Wrote(Err(error)) if error.kind() == ErrorKind::BrokenPipe => return Ok(()),
                Wake::Wrote(Err(error)) => return Err(format!("cannot send a reply: {error}")),
                Wake::Stalled => {
                    return Err(format!(
                        "it made no progress for {} ms",
                        STALL_DEADLINE.as_millis()
                    ));
                }
            }
        }
    }

    /// Answers each complete line in `pending` while the queue has room for the reply and no change waits to be queued.
    async fn answer(&mut self, hub: &Hub) -> Result<(), String> {
        while self.queue.len() < QUEUE_LINES {
            // A subscriber's own sets would otherwise fill its hand-off from the hub before it is drained.
            if self
                .changes
                .as_ref()
                .is_some_and(|changes| !changes.is_empty())
            {
                return Ok(());
            }
            let Some(line) = take_line(&mut self.pending)? else {
                return Ok(());
            };
            self.stall = None;
            let reply = match request::parse(&line) {
                Ok((id, Request::Get)) => reply::state(id, &hub.state()),
                Ok((id, Request::Subscribe)) => {
                    let (state, changes) = hub.follow();
                    self.changes = Some(changes);
                    reply::state(id, &state)
                }
                Ok((id, Request::Submit(command))) => {
                    let outcome = hub.submit(command).await;
                    // One change per turn, so every subscriber's task runs before this client makes the next one.
                    task::yield_now().await;
                    reply::outcome(id, outcome)
                }
                Err(refusal) => reply::refusal(refusal.id, refusal.error),
            };
            self.queue.push_back(reply);
        }
        Ok(())
    }

    /// Queues the event line for one change, or closes when the client has left the queue full.
    fn enqueue_event(&mut self, state: &State) -> Result<(), String> {
        if self.queue.len() >= QUEUE_LINES {
            return Err(format!("it left {QUEUE_LINES} lines unread"));
        }
        self.queue.push_back(reply::event(state));
        Ok(())
    }

    /// Notes `sent` more bytes of the first queued line as written, dropping the line once all of it is.
    fn advance(&mut self, sent: usize) -> Result<(), String> {
        if sent == 0 {
            return Err("the socket took no bytes of a reply".to_owned());
        }
        self.written = self.written.saturating_add(sent);
        if self
            .queue
            .front()
            .is_some_and(|line| self.written >= line.len())
        {
            drop(self.queue.pop_front());
            self.written = 0;
            self.stall = None;
        }
        Ok(())
    }

    /// Whether the client ended its side and was sent everything owed; `answer` leaves complete lines only while a change waits or the queue is full.
    fn owes_nothing_after_its_end(&self) -> bool {
        self.ended
            && self.queue.is_empty()
            && self.changes.as_ref().is_none_or(mpsc::Receiver::is_empty)
    }

    /// Starts the stall deadline when the connection waits on its client, and stops it while it waits only for changes.
    fn watch_for_stall(&mut self) {
        let waits_on_client =
            self.changes.is_none() || !self.pending.is_empty() || !self.queue.is_empty();
        if !waits_on_client {
            self.stall = None;
        } else if self.stall.is_none() {
            self.stall = Some(Box::pin(sleep(STALL_DEADLINE)));
        }
    }

    /// Polls, in order, the write of the first queued line, the next change, the stall deadline and the next request bytes.
    fn poll_wake(&mut self, context: &mut Context<'_>) -> Poll<Wake> {
        if let Some(line) = self.queue.front() {
            let unsent = line.as_bytes().get(self.written..).unwrap_or_default();
            if let Poll::Ready(result) = Pin::new(&mut self.stream).poll_write(context, unsent) {
                return Poll::Ready(Wake::Wrote(result));
            }
        }
        if let Some(changes) = &mut self.changes
            && let Poll::Ready(change) = changes.poll_recv(context)
        {
            return Poll::Ready(Wake::Changed(change));
        }
        if let Some(stall) = &mut self.stall
            && stall.as_mut().poll(context).is_ready()
        {
            return Poll::Ready(Wake::Stalled);
        }
        if self.ended {
            return Poll::Pending;
        }
        let room = PENDING_BYTES.saturating_sub(self.pending.len());
        let Some(window) = self
            .chunk
            .get_mut(..room)
            .filter(|window| !window.is_empty())
        else {
            return Poll::Pending;
        };
        let mut buffer = ReadBuf::new(window);
        match Pin::new(&mut self.stream).poll_read(context, &mut buffer) {
            Poll::Ready(result) => {
                Poll::Ready(Wake::Received(result.map(|()| buffer.filled().len())))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Takes the first complete line out of `pending`, newline excluded, or fails once a line is past `MAX_LINE_BYTES`.
// Sample input: `{"v":1,"id":7,"op":"get"}\n{"v":1,` gives the first object and keeps `{"v":1,`.
fn take_line(pending: &mut Vec<u8>) -> Result<Option<Vec<u8>>, String> {
    match pending.iter().position(|&byte| byte == b'\n') {
        Some(end) if end <= MAX_LINE_BYTES => {
            let rest = pending.split_off(end.saturating_add(1));
            let mut line = mem::replace(pending, rest);
            line.truncate(end);
            Ok(Some(line))
        }
        None if pending.len() <= MAX_LINE_BYTES => Ok(None),
        Some(_) | None => Err(format!(
            "it sent a request line longer than {MAX_LINE_BYTES} bytes"
        )),
    }
}

#[cfg(test)]
mod tests;
