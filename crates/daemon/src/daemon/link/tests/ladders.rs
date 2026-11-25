//! Each rung of the reconnect ladder on the driver's paused clock, where a loaded host cannot stretch a wait.

use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::Duration;

use pods_core::command::{Command, Outcome};
use pods_core::prefs::MicPref;
use pods_fake::{Fault, Pods, Profile, pair, unwatchable};
use pods_sys::l2cap::{Channel, ChannelError};
use rustix::io::Errno;
use tokio::time::{Instant, timeout};

use super::super::Connector;
use super::{HANG_LIMIT, Running, block_on, device_up};

/// The first held rungs, written out so a drifted ladder constant goes red; the rest of the table is the pure machine's to prove.
const HELD_MS: [u64; 3] = [1000, 2000, 4000];

/// One dial, then one per rung.
const DIALS: usize = 4;

/// How long an opened channel waits for the handshake's acknowledgement, written out like `HELD_MS`.
const HANDSHAKE_MS: u64 = 2000;

/// The wait before dialling again a link that was up, written out like `HELD_MS`.
const FIRST_RETRY_MS: u64 = 750;

/// The driver on a paused clock, and the time on that clock of each connect answer as the driver takes it.
fn paused(connector: Connector) -> (Running, std_mpsc::Receiver<Instant>) {
    let (stamps, stamped) = std_mpsc::channel();
    let running = Running::paused(
        connector,
        Box::new(move |_| {
            // A test that has its dials no longer listens.
            stamps.send(Instant::now()).ok();
        }),
    );
    (running, stamped)
}

fn gaps(stamped: &std_mpsc::Receiver<Instant>, dials: usize) -> Vec<Duration> {
    let times: Vec<Instant> = (0..dials)
        .map(|dial| {
            stamped
                .recv_timeout(HANG_LIMIT)
                .unwrap_or_else(|_| panic!("dial {dial} from 0 never came"))
        })
        .collect();
    times
        .windows(2)
        .map(|pair| pair[1].duration_since(pair[0]))
        .collect()
}

/// Each held rung plus `extra`.
fn held_plus(extra: u64) -> Vec<Duration> {
    HELD_MS
        .iter()
        .map(|delay| Duration::from_millis(delay.saturating_add(extra)))
        .collect()
}

#[test]
fn refused_dials_wait_each_held_rung() {
    let refuse: Connector = Arc::new(|_| Err(ChannelError::Connect(Errno::HOSTDOWN)));
    let (running, stamped) = paused(refuse);
    device_up(running.inputs());
    assert_eq!(gaps(&stamped, DIALS), held_plus(0));
}

#[test]
fn channels_the_pods_close_at_once_wait_each_held_rung() {
    let closed: Connector = Arc::new(|_| {
        let (host_end, pods_end) = pair();
        drop(pods_end);
        Ok(host_end)
    });
    let (running, stamped) = paused(closed);
    device_up(running.inputs());
    assert_eq!(gaps(&stamped, DIALS), held_plus(0));
}

#[test]
fn a_channel_the_runtime_cannot_watch_waits_each_held_rung() {
    let (running, stamped) = paused(Arc::new(|_| Ok(unwatchable())));
    device_up(running.inputs());
    assert_eq!(gaps(&stamped, DIALS), held_plus(0));
    assert!(!running.hub().state().connected);
}

#[test]
fn unacknowledged_channels_close_at_the_handshake_step_then_wait_each_held_rung() {
    let held_open: Mutex<Vec<Channel>> = Mutex::default();
    let (running, stamped) = paused(Arc::new(move |_| {
        let (host_end, pods_end) = pair();
        held_open.lock().unwrap().push(pods_end);
        Ok(host_end)
    }));
    device_up(running.inputs());
    assert_eq!(gaps(&stamped, DIALS), held_plus(HANDSHAKE_MS));
}

#[test]
fn a_link_the_pods_hang_up_dials_again_after_the_first_retry() {
    let pods = Pods::new(Profile::default());
    pods.fault(Fault::CloseAfter(10));
    let served = pods.clone();
    let (running, stamped) = paused(Arc::new(move |_| {
        let (host_end, pods_end) = pair();
        served.serve(pods_end);
        Ok(host_end)
    }));
    let hold = running.hold_clock();
    let hub = running.hub();
    // The paused clock can advance to a restore deadline while the connector thread returns.
    assert_eq!(
        block_on(hub.submit(Command::SetMicrophone(MicPref::Off))),
        Outcome::Confirmed
    );
    let (_, mut changes) = hub.follow();
    device_up(running.inputs());
    block_on(async {
        let mut was_up = false;
        loop {
            let change = timeout(HANG_LIMIT, changes.recv()).await.unwrap().unwrap();
            was_up |= change.connected;
            if was_up && !change.connected {
                return;
            }
        }
    });
    pods.fault(Fault::None);
    drop(hold);
    let first_retry = Duration::from_millis(FIRST_RETRY_MS);
    assert_eq!(gaps(&stamped, 2), [first_retry]);
    drop(running);
    pods.finish().unwrap();
}
