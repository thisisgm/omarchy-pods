//! The signals that stop the daemon cleanly, the set `daemon/main.cpp` installs.

use std::future::poll_fn;
use std::task::Poll;

use tokio::signal::unix::{Signal, SignalKind, signal};

/// Each stop signal with the name the log gives it.
const STOP_SIGNALS: [(SignalKind, &str); 3] = [
    (SignalKind::terminate(), "SIGTERM"),
    (SignalKind::interrupt(), "SIGINT"),
    (SignalKind::hangup(), "SIGHUP"),
];

/// Listeners for every stop signal, installed so none of them kills the process before cleanup.
#[derive(Debug)]
pub(super) struct StopSignals {
    listeners: Vec<(Signal, &'static str)>,
}

/// Installs the listeners; until this runs, a stop signal ends the process with the files left behind.
pub(super) fn listen() -> Result<StopSignals, String> {
    let mut listeners = Vec::with_capacity(STOP_SIGNALS.len());
    for (kind, name) in STOP_SIGNALS {
        let listener =
            signal(kind).map_err(|error| format!("cannot listen for {name}: {error}"))?;
        listeners.push((listener, name));
    }
    Ok(StopSignals { listeners })
}

impl StopSignals {
    /// Waits for the first stop signal and returns its name.
    pub(super) async fn first(&mut self) -> &'static str {
        poll_fn(|context| {
            for (listener, name) in &mut self.listeners {
                // A listener that reports closed has lost its driver, which only a stopping runtime does.
                if listener.poll_recv(context).is_ready() {
                    return Poll::Ready(*name);
                }
            }
            Poll::Pending
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use cpp_source::MAIN;

    use super::STOP_SIGNALS;

    #[test]
    fn stop_signals_are_the_ones_the_cpp_installs() {
        let rust: Vec<&str> = STOP_SIGNALS.iter().map(|(_, name)| *name).collect();
        let cpp: Vec<&str> = MAIN
            .all_between("::sigaction(", ",")
            .into_iter()
            .map(str::trim)
            .collect();
        assert_eq!(rust, cpp);
    }
}
