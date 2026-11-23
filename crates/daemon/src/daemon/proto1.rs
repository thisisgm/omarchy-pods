//! One protocol 1 client: replies in order; after `subscribe`, one event line per state change.

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

/// Newline excluded; a longer one closes the connection.
const MAX_LINE_BYTES: usize = 4096;

/// One line at the bound plus the byte that shows it is past it.
const PENDING_BYTES: usize = MAX_LINE_BYTES + 1;

/// An event that finds the queue full closes the connection instead of being skipped.
const QUEUE_LINES: usize = 16;

/// For a line unfinished, a reply unread or an idle unsubscribed client; twice the legacy deadline.
const STALL_DEADLINE: Duration = Duration::from_millis(2000);

#[derive(Debug)]
enum Wake {
    /// Bytes now at the end of `pending`; zero is the end of the stream.
    Received(io::Result<usize>),
    /// `None` when the hub dropped this subscriber for falling behind.
    Changed(Option<Box<State>>),
    /// Bytes of the first queued line taken by the socket.
    Wrote(io::Result<usize>),
    Stalled,
}

#[derive(Debug)]
struct Connection {
    stream: UnixStream,
    /// At most `PENDING_BYTES`.
    pending: Vec<u8>,
    chunk: [u8; PENDING_BYTES],
    /// At most `QUEUE_LINES`.
    queue: VecDeque<String>,
    /// How much of the first queued line the socket has taken.
    written: usize,
    changes: Option<mpsc::Receiver<State>>,
    /// After the client's end no request is read; the connection closes once the queue is written.
    ended: bool,
    /// Runs while waiting on the client; restarts at each answered request and written line.
    stall: Option<Pin<Box<Sleep>>>,
}

/// `received` holds the client's first bytes, already read.
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

    /// Only while the queue has room for the reply and no change waits to be queued.
    async fn answer(&mut self, hub: &Hub) -> Result<(), String> {
        while self.queue.len() < QUEUE_LINES {
            // Else a subscriber's own sets would fill its hand-off from the hub before it drains.
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
                    // One change per turn: all subscriber tasks run before this client's next one.
                    task::yield_now().await;
                    reply::outcome(id, outcome)
                }
                Err(refusal) => reply::refusal(refusal.id, refusal.error),
            };
            self.queue.push_back(reply);
        }
        Ok(())
    }

    fn enqueue_event(&mut self, state: &State) -> Result<(), String> {
        if self.queue.len() >= QUEUE_LINES {
            return Err(format!("it left {QUEUE_LINES} lines unread"));
        }
        self.queue.push_back(reply::event(state));
        Ok(())
    }

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

    /// `answer` leaves complete lines only while a change waits or the queue is full.
    fn owes_nothing_after_its_end(&self) -> bool {
        self.ended
            && self.queue.is_empty()
            && self.changes.as_ref().is_none_or(mpsc::Receiver::is_empty)
    }

    /// Stopped while the connection waits only for changes.
    fn watch_for_stall(&mut self) {
        let waits_on_client =
            self.changes.is_none() || !self.pending.is_empty() || !self.queue.is_empty();
        if !waits_on_client {
            self.stall = None;
        } else if self.stall.is_none() {
            self.stall = Some(Box::pin(sleep(STALL_DEADLINE)));
        }
    }

    /// In order: the first line's write, the next change, the stall deadline, then request bytes.
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
            return Poll::Ready(Wake::Changed(change.map(Box::new)));
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
