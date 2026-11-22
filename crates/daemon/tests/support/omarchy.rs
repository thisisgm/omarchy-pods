//! A stand-in `omarchy` in the session's `bin` directory that records the arguments of every run.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::Duration;

use super::{Session, pause, wait_within};

/// How long a hung stand-in sleeps before it exits by itself, under the 10 s a stand-in may live.
pub(crate) const HANG: Duration = Duration::from_secs(8);

pub(crate) const PROGRAM: &str = "omarchy";

/// The daemon's wait for one run before it kills it.
pub(crate) const NOTIFY_WAIT: Duration = Duration::from_millis(5000);

/// Longer than a run the daemon started takes to reach the record.
pub(crate) const RECORDED_WITHIN: Duration = Duration::from_millis(500);

/// The run the daemon makes for the disconnect notice, the arguments of the C++ `Notifier` and the text of its `main.cpp`.
pub(crate) const DISCONNECT_NOTICE: [&str; 8] = [
    "notification",
    "send",
    "--app-name",
    "AirPods",
    "-g",
    "\u{f025}",
    "AirPods Disconnected",
    "Your AirPods have been disconnected",
];
/// What a failing stand-in says on its standard error before it exits.
pub(crate) const REFUSAL: &str = "omarchy: refused";
const RECORD: &str = "omarchy.calls";
/// Written whole beside `bin` and renamed into it, so a run never finds half a script.
const NEXT: &str = "omarchy.next";
/// The line that ends each run in the record, after its argument count and its arguments.
const END_OF_RUN: &str = "--";
const RUNNABLE: u32 = 0o700;

#[derive(Clone, Copy)]
pub(crate) enum Mood {
    Works,
    Fails,
    Hangs,
    Absent,
}

pub(crate) struct Omarchy {
    program: PathBuf,
    next: PathBuf,
    record: PathBuf,
}

impl Omarchy {
    pub(crate) fn install(session: &Session, mood: Mood) -> Self {
        let omarchy = Self {
            program: session.bin().join(PROGRAM),
            next: session.dir.path().join(NEXT),
            record: session.dir.path().join(RECORD),
        };
        omarchy.become_(mood);
        omarchy
    }

    pub(crate) fn become_(&self, mood: Mood) {
        let after = match mood {
            Mood::Works => String::new(),
            Mood::Fails => format!("echo '{REFUSAL}' >&2\nexit 1\n"),
            // `exec` keeps one process, so the guard that kills the stand-in kills the sleep too.
            Mood::Hangs => format!("exec /usr/bin/sleep {}\n", HANG.as_secs()),
            Mood::Absent => {
                if let Err(error) = fs::remove_file(&self.program) {
                    assert_eq!(error.kind(), ErrorKind::NotFound, "{error}");
                }
                return;
            }
        };
        let record = self.record.to_str().unwrap();
        assert!(
            !record.contains('\''),
            "a quote in the record path {record}"
        );
        let script = format!(
            concat!(
                "#!/bin/sh\n",
                "run=\n",
                "for argument in \"$@\"; do\n",
                "  run=\"$run$argument\n\"\n",
                "done\n",
                "printf '%s\\n%s{end}\\n' \"$#\" \"$run\" >> '{record}'\n",
                "{after}",
            ),
            end = END_OF_RUN,
            record = record,
            after = after,
        );
        let mut next = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(RUNNABLE)
            .open(&self.next)
            .unwrap();
        next.write_all(script.as_bytes()).unwrap();
        drop(next);
        fs::rename(&self.next, &self.program).unwrap();
    }

    /// One entry per finished run, its arguments in order; sample record `3\nnotification\nsend\nhello\n--\n`.
    pub(crate) fn calls(&self) -> Vec<Vec<String>> {
        let text = match fs::read_to_string(&self.record) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
            Err(error) => panic!("reading {}: {error}", self.record.display()),
        };
        let mut lines = text.split_terminator('\n');
        let mut calls = Vec::new();
        while let Some(count) = lines.next() {
            let count: usize = count
                .parse()
                .unwrap_or_else(|error| panic!("an argument count `{count}`: {error}: {text:?}"));
            let run = lines.by_ref().take(count).map(str::to_owned).collect();
            match lines.next() {
                Some(END_OF_RUN) => calls.push(run),
                None => break,
                Some(_) => panic!("an argument held a newline: {text:?}"),
            }
        }
        calls
    }

    /// Waits for `count` runs in all, failing once `within` passes.
    pub(crate) fn wait_calls(&self, count: usize, within: Duration) -> Vec<Vec<String>> {
        wait_within(&format!("{count} runs of {PROGRAM}"), within, || {
            let calls = self.calls();
            (calls.len() >= count).then_some(calls)
        })
    }

    /// Fails with the record if any run was recorded once `quiet` has passed.
    pub(crate) fn never_ran(&self, quiet: Duration) {
        pause(quiet);
        let calls = self.calls();
        assert!(calls.is_empty(), "{PROGRAM} ran: {calls:?}");
    }

    /// Fails with the record if a run comes within `quiet`.
    pub(crate) fn none_for(&self, quiet: Duration) {
        let before = self.calls().len();
        pause(quiet);
        let calls = self.calls();
        assert_eq!(
            calls.len(),
            before,
            "{PROGRAM} ran within {quiet:?}: {calls:?}"
        );
    }
}
