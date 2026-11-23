//! Shows the policy's notices on the desktop through the stock `omarchy notification send`, one run at a time.

use std::fmt::Display;
use std::future::poll_fn;
use std::io::{self, ErrorKind};
use std::pin::{Pin, pin};
use std::process::{ExitStatus, Stdio};
use std::task::Poll;
use std::time::Duration;

use pods_core::policy::notices::Notice;
use pods_core::prefs::Prefs;
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::link::STOP_WAIT_MS;
use super::log;

/// Three latches and a disconnect cannot exceed it; one more is dropped and logged.
pub(super) const NOTICE_QUEUE: usize = 8;

/// A judgment: a notifier that takes longer is hung, and is killed and reaped.
const NOTIFY_WAIT_MS: u64 = 5000;

/// Found through `PATH`, as the C++ daemon finds it.
const PROGRAM: &str = "omarchy";

/// Before the title and the message, as the C++ daemon's `Notifier` passes them.
const ARGS: [&str; 6] = [
    "notification",
    "send",
    "--app-name",
    "AirPods",
    "-g",
    "\u{f025}",
];

const QUEUE_FULL: &str = "notify: queue full";
const NOT_FOUND: &str = "notify: omarchy not found";
const FAILED: &str = "notify: omarchy failed";
const HUNG: &str = "notify: omarchy hung and was killed";

/// How one run of the program ended.
enum Run {
    Exited(io::Result<ExitStatus>),
    Hung,
    Stopped,
}

/// The failure lines already logged, each logged once per daemon life.
#[derive(Default)]
struct Said(Vec<&'static str>);

impl Said {
    fn once(&mut self, line: &'static str, why: Option<&dyn Display>) {
        if self.0.contains(&line) {
            return;
        }
        match why {
            Some(why) => log(&format!("{line}: {why}")),
            None => log(line),
        }
        self.0.push(line);
    }
}

/// The one place the queue's bound is set.
pub(super) fn queue() -> (mpsc::Sender<Notice>, mpsc::Receiver<Notice>) {
    mpsc::channel(NOTICE_QUEUE)
}

/// Queues `notice` without waiting; a full queue drops it with a log line.
pub(super) fn offer(notices: &mpsc::Sender<Notice>, notice: Notice) {
    // A closed queue is a notifier that already stopped, which only a stopping daemon has.
    if let Err(TrySendError::Full(_)) = notices.try_send(notice) {
        log(QUEUE_FULL);
    }
}

/// Shows each notice in turn, dropping it while `prefs` has notices off, until `stop` fires or its sender is dropped.
pub(super) async fn run(
    mut notices: mpsc::Receiver<Notice>,
    prefs: watch::Receiver<Prefs>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut said = Said::default();
    while let Some(notice) = next(&mut notices, &mut stop).await {
        // At the send, as the C++ `Notifier` checks, so a notice queued before the switch went off never starts.
        if !prefs.borrow().notifications {
            continue;
        }
        let Some(mut child) = start(notice, &mut said) else {
            continue;
        };
        match wait(&mut child, &mut stop).await {
            Run::Exited(Ok(status)) if status.success() => {}
            Run::Exited(Ok(status)) => said.once(FAILED, Some(&status)),
            Run::Exited(Err(error)) => said.once(FAILED, Some(&error)),
            Run::Hung => {
                // Kills and reaps; an error is a run already reaped, which is the end wanted.
                child.kill().await.ok();
                said.once(HUNG, None);
            }
            Run::Stopped => {
                child.kill().await.ok();
                return;
            }
        }
    }
}

/// Ends the notifier inside `STOP_WAIT_MS`, a run still going killed and reaped first.
pub(super) async fn stop(stopping: oneshot::Sender<()>, mut notifier: JoinHandle<()>) {
    // An error is a notifier that already ended, which is stopped all the same.
    stopping.send(()).ok();
    if timeout(Duration::from_millis(STOP_WAIT_MS), &mut notifier)
        .await
        .is_err()
    {
        log(&format!(
            "notify: the notifier did not stop within {STOP_WAIT_MS} ms; aborting it"
        ));
        notifier.abort();
        // Awaited so the run is dropped, and killed, first; the result is only the cancellation.
        drop(notifier.await);
    }
}

/// The next notice, or none once the daemon stops.
async fn next(
    notices: &mut mpsc::Receiver<Notice>,
    stop: &mut oneshot::Receiver<()>,
) -> Option<Notice> {
    poll_fn(|context| {
        if Pin::new(&mut *stop).poll(context).is_ready() {
            return Poll::Ready(None);
        }
        notices.poll_recv(context)
    })
    .await
}

/// One run for `notice`, or none with the reason logged.
fn start(notice: Notice, said: &mut Said) -> Option<Child> {
    let spawned = Command::new(PROGRAM)
        .args(ARGS)
        .arg(notice.title())
        .arg(notice.message())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // The program's own words about a failure reach the daemon's log.
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn();
    match spawned {
        Ok(child) => Some(child),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            said.once(NOT_FOUND, None);
            None
        }
        Err(error) => {
            said.once(FAILED, Some(&error));
            None
        }
    }
}

/// The run's exit within `NOTIFY_WAIT_MS`, unless the daemon stops first.
async fn wait(child: &mut Child, stop: &mut oneshot::Receiver<()>) -> Run {
    let mut exited = pin!(timeout(Duration::from_millis(NOTIFY_WAIT_MS), child.wait()));
    poll_fn(|context| {
        if Pin::new(&mut *stop).poll(context).is_ready() {
            return Poll::Ready(Run::Stopped);
        }
        exited
            .as_mut()
            .poll(context)
            .map(|waited| waited.map_or(Run::Hung, Run::Exited))
    })
    .await
}

#[cfg(test)]
mod tests {
    //! Support tests: the program and its arguments pinned to the C++ daemon's `Notifier`, and the wait for a run on a paused clock.

    use std::time::Duration;

    use cpp_source::NOTIFIER;
    use tokio::process::Command;
    use tokio::sync::oneshot;
    use tokio::time::Instant;

    use super::{ARGS, PROGRAM, Run, wait};
    use crate::daemon::paused::on_paused_clock;

    /// `NOTIFY_WAIT_MS`, written out so a drifted wait goes red.
    const NOTIFY_WAIT: Duration = Duration::from_millis(5000);

    /// Each `QStringLiteral("...")` in `call`, a `\uXXXX` escape read as its character; sample input `{QStringLiteral("-g"), QStringLiteral(""), title, message}`.
    fn literals(call: &str) -> Vec<String> {
        call.split("QStringLiteral(\"")
            .skip(1)
            .map(|piece| {
                let literal = piece.split_once("\")").unwrap().0;
                match literal.strip_prefix("\\u") {
                    Some(hex) => char::from_u32(u32::from_str_radix(hex, 16).unwrap())
                        .unwrap()
                        .to_string(),
                    None => literal.to_owned(),
                }
            })
            .collect()
    }

    #[test]
    fn the_program_and_its_arguments_are_the_cpp_notifiers() {
        let call = NOTIFIER.between("QProcess::startDetached(omarchy, {", "title, message});");
        assert_eq!(literals(call), ARGS);
        let program = NOTIFIER.find(None, "findExecutable(QStringLiteral(\"", '"');
        assert_eq!(program, PROGRAM);
    }

    #[test]
    fn a_run_that_never_exits_is_hung_after_the_wait() {
        on_paused_clock(async {
            let mut child = Command::new("sleep")
                .arg("600")
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let (_stop, mut stopped) = oneshot::channel();
            let started = Instant::now();
            let run = wait(&mut child, &mut stopped).await;
            assert!(matches!(run, Run::Hung), "the run ended otherwise");
            assert_eq!(started.elapsed(), NOTIFY_WAIT);
        });
    }
}
