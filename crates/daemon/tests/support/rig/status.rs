//! The rig's client, control socket status and waits on published daemon state.

use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::super::audio::{Asked as AudioAsked, PERCENT};
use super::super::watch::Watch;
use super::super::{POLL, binary, pause, wait_for};
use super::{Rig, connected, named};

/// A sink no test names, added and removed to make the audio adapter refresh without changing the pods' sink.
const PASSING_SINK: &str = "alsa_output.passing";

impl Rig {
    /// The `pods` client in this session, run to its end.
    pub(crate) fn client(&self, args: &[&str]) -> Output {
        self.session.run_client(Path::new(binary()), args)
    }

    /// The control socket's status document, parsed.
    pub(crate) fn status(&self) -> Value {
        // Sample input: `{"schema_version":1,"connected":false}\n`.
        serde_json::from_str(&self.session.status()).expect("the status socket returns JSON")
    }

    /// What `pods status` prints, parsed.
    pub(crate) fn cli_status(&self) -> Value {
        let output = self.client(&["status"]);
        assert!(output.status.success(), "pods status: {output:?}");
        // Sample input: `{"schema_version":1,"connected":false}\n`.
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// `pods watch` in this session, its lines read as they come.
    pub(crate) fn watch(&self) -> Watch {
        Watch::start(&self.session)
    }

    /// Reads status until `ready` holds, failing at `HANG_LIMIT`.
    pub(crate) fn wait_status(&self, what: &str, ready: impl Fn(&Value) -> bool) -> Value {
        wait_for(what, || {
            let status = self.status();
            ready(&status).then_some(status)
        })
    }

    /// Reads status until `audio_server` reads `word`.
    pub(crate) fn wait_audio_server(&self, word: &str) -> Value {
        self.wait_status(&format!("audio_server {word}"), |status| {
            status["audio_server"] == word
        })
    }

    /// Waits for `up`, for a refresh after a sink came and went (asked only once the sink read at `up` was queued), then for the driver to apply it.
    pub(crate) fn wait_sink_told(&self) {
        self.wait_audio_server("up");
        let audio = self.audio.as_ref().expect("the fake audio server runs");
        let before = audio.requests().len();
        audio.add_sink(PASSING_SINK, PERCENT);
        audio.remove_sink(PASSING_SINK);
        wait_for("a refresh after a sink came and went", || {
            audio.requests()[before..]
                .contains(&AudioAsked::SinkList)
                .then_some(())
        });
        self.wait_queued_inputs_applied();
    }

    /// Returns once the driver applied every input queued before the call, as a command is answered from that queue and a status from the published state; `reopen` only counts `reopen_calls_total`.
    pub(crate) fn wait_queued_inputs_applied(&self) {
        let reply = self.session.ask(b"reopen");
        assert!(
            !reply.is_empty(),
            "reopen, the barrier behind the queued inputs: no reply, so the driver did not answer it"
        );
    }

    /// Reads status for `duration`, failing at the first answer where `check` does not hold; the first is checked whenever it comes, as every caller's state outlasts the window.
    pub(crate) fn holds_for(&self, duration: Duration, what: &str, check: impl Fn(&Value) -> bool) {
        let started = Instant::now();
        loop {
            let status = self.status();
            assert!(
                check(&status),
                "{what} ended after {:?}: {status}",
                started.elapsed()
            );
            if started.elapsed() >= duration {
                return;
            }
            pause(POLL);
        }
    }

    /// Waits for the link up and the pods' name in the status, which the burst brings after the batteries and the ears.
    pub(crate) fn wait_filled(&self) -> Value {
        self.wait_status("the link up and the pods' name", |status| {
            connected(status) && named(status)
        })
    }

    /// Takes `do disconnect`, then waits for the name to clear, which only the reset does.
    pub(crate) fn disconnect(&self) {
        let output = self.client(&["do", "disconnect"]);
        assert!(output.status.success(), "do disconnect: {output:?}");
        self.wait_status("the name cleared", |status| status["device_name"] == "");
    }
}
