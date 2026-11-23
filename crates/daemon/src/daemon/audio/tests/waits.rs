//! The adapter's waits on a paused clock, against a server that accepts each connection and never answers.

use std::fs;
use std::time::Duration;

use pods_core::policy::audio::AudioRequest;
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::super::{Input, SOCKET, queue, run, stop};
use super::DEVICE;
use crate::daemon::paused::on_paused_clock;

/// `AUDIO_CALL_TIMEOUT_MS`, written out so a drifted wait goes red.
const CALL_TIMEOUT_MS: u64 = 2000;

/// `AUDIO_RETRY_MS` then its last step again, written out like `CALL_TIMEOUT_MS`.
const LADDER_MS: [u64; 6] = [250, 500, 1000, 2000, 5000, 5000];

/// Room for the one `Down` an outage tells the driver.
const FACTS: usize = 8;

/// The adapter following the device to a silent server, and the paused time of each connection it made.
struct Following {
    requests: mpsc::Sender<AudioRequest>,
    adapter: JoinHandle<()>,
    accepted: mpsc::UnboundedReceiver<Instant>,
    _facts: mpsc::Receiver<Input>,
    _dir: TempDir,
}

/// Inside a paused runtime: a server at the adapter's socket that keeps every connection unanswered.
async fn follow_a_silent_server() -> Following {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join(SOCKET);
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let (stamps, accepted) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            // A test that has its connections no longer listens.
            stamps.send(Instant::now()).ok();
            held.push(stream);
        }
    });
    let (inputs, facts) = mpsc::channel(FACTS);
    let (requests, asked) = queue();
    let adapter = tokio::spawn(run(inputs, asked, dir.path().to_path_buf()));
    let device = DEVICE.parse().unwrap();
    requests
        .send(AudioRequest::Follow(Some(device)))
        .await
        .unwrap();
    Following {
        requests,
        adapter,
        accepted,
        _facts: facts,
        _dir: dir,
    }
}

#[test]
fn a_silent_server_is_dialled_again_after_each_call_timeout_and_step() {
    let gaps = on_paused_clock(async {
        let mut following = follow_a_silent_server().await;
        let mut times = Vec::new();
        for _ in 0..=LADDER_MS.len() {
            times.push(following.accepted.recv().await.unwrap());
        }
        let gaps: Vec<Duration> = times
            .windows(2)
            .map(|pair| pair[1].duration_since(pair[0]))
            .collect();
        gaps
    });
    let wanted: Vec<Duration> = LADDER_MS
        .iter()
        .map(|step| Duration::from_millis(CALL_TIMEOUT_MS.saturating_add(*step)))
        .collect();
    assert_eq!(gaps, wanted);
}

#[test]
fn a_silent_server_does_not_delay_the_stop() {
    let waited = on_paused_clock(async {
        let mut following = follow_a_silent_server().await;
        following.accepted.recv().await.unwrap();
        drop(following.requests);
        let started = Instant::now();
        stop(following.adapter).await;
        started.elapsed()
    });
    assert_eq!(waited, Duration::ZERO);
}
