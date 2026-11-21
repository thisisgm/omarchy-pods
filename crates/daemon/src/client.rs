//! The command line client, byte for byte the C++ `daemon/librepods-ctl.cpp`.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How long the client waits to connect, as `waitForConnected(500)` does.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// How long the client waits to hand the verb over, as `waitForBytesWritten(500)` does.
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long the client waits for a reply, as `waitForReadyRead(1000)` does.
const REPLY_TIMEOUT: Duration = Duration::from_millis(1000);

/// Upper bound on a reply, so a peer that never sends a newline cannot grow memory.
const MAX_REPLY_BYTES: usize = 65536;

/// Size of one read from the socket.
const READ_CHUNK_BYTES: usize = 4096;

/// The verb whose reply is a status line.
const STATUS_VERB: &str = "status";

/// The only other verb the daemon answers, and a silent close is its yes.
const REOPEN_VERB: &str = "reopen";

/// A reply starting with this is a refusal and goes to stderr.
const ERROR_PREFIX: &[u8] = b"error:";

/// The usage block of `librepods-ctl.cpp`, after its first line.
const USAGE_COMMANDS: &str = "Commands:
  status              Print one-line JSON status snapshot to stdout
  noise:off           Disable noise control
  noise:anc           Enable Active Noise Cancellation
  noise:transparency  Enable Transparency mode
  noise:adaptive      Enable Adaptive mode
  noise:cycle         Advance to the next mode (Off->ANC->Trans->Adaptive)
  ear:off             Disable automatic ear-detection auto-pause/resume
  ear:one             Pause when either pod is removed (default)
  ear:both            Pause only when both pods are removed
  forget              Run `bluetoothctl remove` on the connected device
  ca:on               Enable Conversation Awareness (Pro2 only)
  ca:off              Disable Conversation Awareness
  disconnect          bluetoothctl disconnect on the paired AirPods
  connect             bluetoothctl connect on the paired AirPods
  adaptive:N          Set Adaptive Noise level 0-100 (Pro2/Pro3, only while noise_mode=Adaptive)
  onebud:on           Enable One-Bud ANC (Pro2+: keep ANC active with only one pod in)
  onebud:off          Disable One-Bud ANC
";

/// How the wait for a reply ended.
#[derive(Debug)]
enum Reply {
    /// Some bytes arrived: up to the first newline, the end of stream, the size bound or the deadline.
    Received(Vec<u8>),
    /// The peer closed or reset the connection before sending anything.
    Closed,
    /// Nothing arrived before the deadline.
    TimedOut,
}

/// Prints the client usage under `program` and fails.
pub(crate) fn usage(program: &str, err: &mut dyn Write) -> ExitCode {
    emit(
        err,
        format!("Usage: {program} <command>\n{USAGE_COMMANDS}").as_bytes(),
    );
    ExitCode::FAILURE
}

/// Reports a session with no runtime directory, where no socket can be located.
pub(crate) fn no_runtime_dir(err: &mut dyn Write) -> ExitCode {
    emit(
        err,
        b"XDG_RUNTIME_DIR is unset; cannot locate the daemon socket\n",
    );
    ExitCode::FAILURE
}

/// Sends `verb` to the daemon at `socket` and, for `status` and `reopen`, relays the reply.
pub(crate) fn run_legacy(
    verb: &str,
    socket: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let Some(mut stream) = connect(socket) else {
        let mut message = b"Could not connect to OpenPods daemon at ".to_vec();
        message.extend_from_slice(socket.as_os_str().as_bytes());
        message.extend_from_slice(b" (is it running?)\n");
        emit(err, &message);
        return ExitCode::FAILURE;
    };
    let sent = stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .and_then(|()| stream.write_all(verb.as_bytes()));
    // The C++ client ignores a failed write too; a verb that wants a reply then times out.
    drop(sent);
    if verb != STATUS_VERB && verb != REOPEN_VERB {
        return ExitCode::SUCCESS;
    }
    match read_reply(&mut stream) {
        Reply::Received(reply) if reply.starts_with(ERROR_PREFIX) => {
            emit(err, &reply);
            ExitCode::FAILURE
        }
        Reply::Received(reply) => {
            emit(out, &reply);
            ExitCode::SUCCESS
        }
        Reply::Closed if verb == REOPEN_VERB => ExitCode::SUCCESS,
        Reply::Closed | Reply::TimedOut => {
            emit(
                err,
                format!("Timed out waiting for a reply to {verb}\n").as_bytes(),
            );
            ExitCode::FAILURE
        }
    }
}

/// Connects within `CONNECT_TIMEOUT`, or gives up.
fn connect(socket: &Path) -> Option<UnixStream> {
    // std has no Unix connect timeout, and a stuck daemon's full backlog blocks connect forever.
    let (sender, receiver) = mpsc::channel();
    let path = socket.to_path_buf();
    thread::Builder::new()
        .spawn(move || sender.send(UnixStream::connect(path)))
        .ok()?;
    receiver.recv_timeout(CONNECT_TIMEOUT).ok()?.ok()
}

/// Reads one reply, bounded in size by `MAX_REPLY_BYTES` and in time by `REPLY_TIMEOUT` overall.
fn read_reply(stream: &mut UnixStream) -> Reply {
    let started = Instant::now();
    let mut reply = Vec::new();
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    loop {
        let remaining = REPLY_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            return settle(reply, Reply::TimedOut);
        }
        let room = MAX_REPLY_BYTES
            .saturating_sub(reply.len())
            .min(READ_CHUNK_BYTES);
        let window = chunk.get_mut(..room).unwrap_or_default();
        match stream.read(window) {
            Ok(0) => return settle(reply, Reply::Closed),
            Ok(read) => {
                let fresh = window.get(..read).unwrap_or_default();
                if let Some(newline) = fresh.iter().position(|&byte| byte == b'\n') {
                    reply.extend_from_slice(fresh.get(..=newline).unwrap_or_default());
                    return Reply::Received(reply);
                }
                reply.extend_from_slice(fresh);
                if reply.len() >= MAX_REPLY_BYTES {
                    return Reply::Received(reply);
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return settle(reply, Reply::TimedOut);
            }
            Err(_) => return settle(reply, Reply::Closed),
        }
    }
}

/// Bytes already received are the reply; with none, the wait ended in `silence`.
fn settle(reply: Vec<u8>, silence: Reply) -> Reply {
    if reply.is_empty() {
        silence
    } else {
        Reply::Received(reply)
    }
}

/// Writes to a terminal stream; a failure there has nowhere left to be reported, as in the C++ client.
fn emit(stream: &mut dyn Write, bytes: &[u8]) {
    drop(stream.write_all(bytes).and_then(|()| stream.flush()));
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::process::ExitCode;
    use std::thread;

    use super::{MAX_REPLY_BYTES, run_legacy};

    /// Runs `status` against a stub that writes `reply` and closes, and returns the exit code, stdout and stderr.
    fn status_with_reply(reply: Vec<u8>) -> (ExitCode, Vec<u8>, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("librepods.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let stub = thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            let mut verb = [0_u8; 16];
            let _ = client.read(&mut verb).unwrap();
            // The client may stop reading early and close, so a refused write is expected here.
            drop(client.write_all(&reply));
        });
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_legacy("status", Path::new(&socket), &mut out, &mut err);
        stub.join().unwrap();
        (code, out, err)
    }

    #[test]
    fn reply_ends_at_the_first_newline() {
        let (code, out, err) = status_with_reply(b"{\"a\":1}\n{\"b\":2}\n".to_vec());
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(out, b"{\"a\":1}\n");
        assert_eq!(err, b"");
    }

    #[test]
    fn reply_without_newline_ends_at_end_of_stream() {
        let (code, out, _) = status_with_reply(b"{\"a\":1}".to_vec());
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(out, b"{\"a\":1}");
    }

    #[test]
    fn reply_is_cut_at_the_size_bound() {
        let (code, out, _) = status_with_reply(vec![b'x'; MAX_REPLY_BYTES * 2]);
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(out.len(), MAX_REPLY_BYTES);
    }
}
