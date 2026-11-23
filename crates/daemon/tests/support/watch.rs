//! `librepods watch` as a child of the test, its printed lines read on a thread.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Daemon, HANG_LIMIT, Session, binary};

const WATCH_LOG_NAME: &str = "watch.log";

/// One line `librepods watch` printed, and when the test's reader took it.
#[derive(Debug)]
pub(crate) struct Printed {
    pub(crate) at: Instant,
    pub(crate) state: Value,
}

/// `librepods watch` in a guard, its output read on a thread the guard joins.
pub(crate) struct Watch {
    process: Daemon,
    lines: Receiver<(Instant, String)>,
    reader: Option<JoinHandle<()>>,
}

impl Watch {
    /// Starts `librepods watch` in `session`, its lines read as they come.
    pub(crate) fn start(session: &Session) -> Self {
        let log = File::create(session.dir.path().join(WATCH_LOG_NAME)).unwrap();
        let mut child = session
            .command(Path::new(binary()), &["watch"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let process = Daemon {
            child,
            log: session.dir.path().join(WATCH_LOG_NAME),
        };
        let (sender, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if sender.send((Instant::now(), line)).is_err() {
                    return;
                }
            }
        });
        Self {
            process,
            lines,
            reader: Some(reader),
        }
    }

    /// The next line, or none at the deadline, a watch that ended failing the test; sample input `{"adaptive_noise_level":50,"case":{"available":true,"charging":false,"level":50},"connected":true,...}`.
    pub(crate) fn next(&self, deadline: Duration) -> Option<Printed> {
        let (at, line) = match self.lines.recv_timeout(deadline) {
            Ok(printed) => printed,
            Err(RecvTimeoutError::Timeout) => return None,
            Err(RecvTimeoutError::Disconnected) => {
                panic!("watch ended: {}", self.process.log())
            }
        };
        let state = serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("watch printed {line:?}: {error}"));
        Some(Printed { at, state })
    }

    /// Every line up to and including the first equal to `last`, failing once `HANG_LIMIT` passes in all.
    pub(crate) fn lines_until(&self, last: &Value) -> Vec<Value> {
        let started = Instant::now();
        let mut lines = Vec::new();
        while lines.last() != Some(last) {
            let left = HANG_LIMIT.saturating_sub(started.elapsed());
            let Some(printed) = self.next(left) else {
                panic!("watch never printed {last}; it printed {lines:#?}");
            };
            lines.push(printed.state);
        }
        lines
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // A watch that already exited refuses the kill, which is the outcome wanted.
        drop(self.process.child.kill());
        drop(self.process.child.wait());
        if let Some(reader) = self.reader.take() {
            // The kill closed the pipe, so the reader is at its end; one that panicked has nothing left to clean up.
            drop(reader.join());
        }
    }
}
