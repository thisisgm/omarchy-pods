//! One client of the C++ daemon's socket dialect: one verb in, at most one line out, then the connection closes.

use std::io::ErrorKind;
use std::time::Duration;

use pods_core::command::{Command, CommandError, Outcome};
use pods_core::legacy::{LegacyRequest, parse_verb};
use pods_core::status::to_status_line;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::{Instant, timeout};

use super::hub::Hub;
use super::log;

/// The most a request may hold; the longest verb is a few dozen bytes.
const MAX_REQUEST_BYTES: usize = 4096;

/// How long after accept the request may take to arrive.
pub(super) const REQUEST_DEADLINE: Duration = Duration::from_millis(1000);

/// How long the client may take to accept the reply.
const REPLY_DEADLINE: Duration = Duration::from_millis(500);

/// The reply to `reopen`, exactly as `daemon/main.cpp` writes it.
const HEADLESS_REFUSAL: &str = "error: this daemon runs headless and has no window\n";

/// The most of an unknown request the log quotes.
const LOGGED_REQUEST_BYTES: usize = 64;

/// Reads the rest of one request after the bytes already in `request`, within what is `left` of the request deadline, answers it if it gets an answer, and closes.
pub(super) async fn serve_connection(
    mut stream: UnixStream,
    hub: Hub,
    mut request: Vec<u8>,
    left: Duration,
) {
    let verb = read_request(&mut stream, &mut request, left).await;
    let Some(reply) = respond(&hub, verb.ok_or(request.as_slice())).await else {
        return;
    };
    match timeout(REPLY_DEADLINE, stream.write_all(reply.as_bytes())).await {
        Ok(Ok(())) => {}
        // A second daemon instance sends `reopen` and exits without reading the refusal, which is normal.
        Ok(Err(error)) if error.kind() == ErrorKind::BrokenPipe => {}
        Ok(Err(error)) => log(&format!("control socket: cannot send the reply: {error}")),
        Err(_elapsed) => log(&format!(
            "control socket: the client took no reply within {REPLY_DEADLINE:?}"
        )),
    }
}

/// Reads until the bytes are a verb answered at once, the client stops sending, `MAX_REQUEST_BYTES`, or `left` runs out, and gives the verb they make.
// Sample input: `status` with the client's write side left open, or `noise:anc` then end of stream.
async fn read_request(
    stream: &mut (impl AsyncRead + Unpin),
    request: &mut Vec<u8>,
    left: Duration,
) -> Option<LegacyRequest> {
    let started = Instant::now();
    let mut chunk = [0_u8; MAX_REQUEST_BYTES];
    loop {
        let verb = parse_verb(request);
        if answered_at_once(verb) {
            return verb;
        }
        let room = MAX_REQUEST_BYTES.saturating_sub(request.len());
        let Some(window) = chunk.get_mut(..room).filter(|window| !window.is_empty()) else {
            return verb;
        };
        // The deadline only ends the wait: whatever arrived by then is parsed as one verb.
        match timeout(left.saturating_sub(started.elapsed()), stream.read(window)).await {
            Ok(Ok(0)) | Err(_) => return verb,
            Ok(Ok(read)) => request.extend_from_slice(window.get(..read).unwrap_or_default()),
            Ok(Err(error)) if error.kind() == ErrorKind::Interrupted => {}
            // A reset client still has what it sent parsed, as the C++ handles the bytes it read.
            Ok(Err(_)) => return verb,
        }
    }
}

/// The legacy client writes these and waits without closing, so they cannot wait for end of stream.
fn answered_at_once(verb: Option<LegacyRequest>) -> bool {
    matches!(
        verb,
        Some(LegacyRequest::Status | LegacyRequest::Command(Command::Reopen))
    )
}

/// The reply to a verb, or to the bytes that made none, if any; only `status` and `reopen` get one.
async fn respond(hub: &Hub, verb: Result<LegacyRequest, &[u8]>) -> Option<String> {
    match verb {
        Ok(LegacyRequest::Status) => Some(to_status_line(&hub.state())),
        Ok(LegacyRequest::Command(command)) => match hub.submit(command).await {
            Outcome::Refused(CommandError::Headless) => Some(HEADLESS_REFUSAL.to_owned()),
            Outcome::Refused(_) | Outcome::Sent | Outcome::Confirmed => None,
        },
        // A client that sent nothing gave the C++ no message to log either.
        Err([]) => None,
        Err(request) => {
            let quoted = request.get(..LOGGED_REQUEST_BYTES).unwrap_or(request);
            log(&format!(
                "control socket: unknown request {:?}",
                String::from_utf8_lossy(quoted)
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use cpp_source::MAIN;
    use proptest::prelude::*;
    use tokio::io::ReadBuf;
    use tokio::runtime::Builder;

    use super::*;

    /// A client that hands over one chunk per read, then ends the stream.
    struct Chunked {
        chunks: Vec<Vec<u8>>,
    }

    impl AsyncRead for Chunked {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.chunks.is_empty() {
                return Poll::Ready(Ok(()));
            }
            let chunk = &mut self.chunks[0];
            let take = chunk.len().min(buf.remaining());
            buf.put_slice(&chunk[..take]);
            chunk.drain(..take);
            if chunk.is_empty() {
                self.chunks.remove(0);
            }
            Poll::Ready(Ok(()))
        }
    }

    fn read_chunks(chunks: Vec<Vec<u8>>) -> Vec<u8> {
        let mut client = Chunked { chunks };
        let mut request = Vec::new();
        let verb = Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(read_request(&mut client, &mut request, REQUEST_DEADLINE));
        assert_eq!(verb, parse_verb(&request));
        request
    }

    /// What the reader must return: the first chunk boundary that makes `status` or `reopen`, else everything up to the bound.
    fn expected(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut seen = Vec::new();
        for chunk in chunks {
            for byte in chunk {
                if seen.len() == MAX_REQUEST_BYTES {
                    return seen;
                }
                seen.push(*byte);
            }
            if seen == b"status" || seen == b"reopen" {
                return seen;
            }
        }
        seen
    }

    fn chunks_of(
        bytes: impl Strategy<Value = u8> + Clone,
        longest: usize,
    ) -> impl Strategy<Value = Vec<Vec<u8>>> {
        proptest::collection::vec(proptest::collection::vec(bytes, 1..longest), 0..8)
    }

    proptest! {
        #[test]
        fn any_chunks_are_read_to_the_end_the_bound_or_an_answered_verb(
            chunks in chunks_of(any::<u8>(), 2 * MAX_REQUEST_BYTES / 3),
        ) {
            prop_assert_eq!(read_chunks(chunks.clone()), expected(&chunks));
        }

        #[test]
        fn chunks_that_start_as_an_answered_verb_stop_there_or_read_on(
            verb in prop_oneof![Just(b"status".to_vec()), Just(b"reopen".to_vec())],
            split in 0..=6_usize,
            tail in chunks_of(any::<u8>(), 16),
        ) {
            let mut chunks = vec![verb[..split].to_vec(), verb[split..].to_vec()];
            chunks.retain(|chunk| !chunk.is_empty());
            chunks.extend(tail);
            prop_assert_eq!(read_chunks(chunks.clone()), expected(&chunks));
        }
    }

    #[test]
    fn status_split_across_reads_is_answered_without_end_of_stream() {
        let chunks = vec![b"sta".to_vec(), b"tus".to_vec(), b"more".to_vec()];
        assert_eq!(read_chunks(chunks), b"status");
    }

    #[test]
    fn status_with_more_in_the_same_read_is_read_to_the_end() {
        assert_eq!(read_chunks(vec![b"statusX".to_vec()]), b"statusX");
    }

    #[test]
    fn a_request_stops_at_the_bound() {
        let request = read_chunks(vec![vec![b'a'; 3 * MAX_REQUEST_BYTES]]);
        assert_eq!(request.len(), MAX_REQUEST_BYTES);
    }

    #[test]
    fn refusal_is_the_cpp_reply() {
        let cpp = MAIN.between("clientSocket->write(\"", "\");");
        assert_eq!(HEADLESS_REFUSAL, cpp.replace("\\n", "\n"));
    }
}
