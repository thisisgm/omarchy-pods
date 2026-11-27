//! The signals that stop the daemon cleanly.

use std::future::poll_fn;
use std::task::Poll;

use tokio::signal::unix::{Signal, SignalKind, signal};

const STOP_SIGNALS: [(SignalKind, &str); 3] = [
    (SignalKind::terminate(), "SIGTERM"),
    (SignalKind::interrupt(), "SIGINT"),
    (SignalKind::hangup(), "SIGHUP"),
];

#[derive(Debug)]
pub(super) struct StopSignals {
    listeners: Vec<(Signal, &'static str)>,
}

/// Until this runs, a stop signal ends the process with the files left behind.
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
    pub(super) async fn first(&mut self) -> &'static str {
        poll_fn(|context| {
            for (listener, name) in &mut self.listeners {
                // Closed means the listener lost its driver, which only a stopping runtime does.
                if listener.poll_recv(context).is_ready() {
                    return Poll::Ready(*name);
                }
            }
            Poll::Pending
        })
        .await
    }
}
