//! What the fake's socket tests share: the host's end of a served channel and bounded reads of it.

use std::time::Duration;

use pods_fake::{Pods, pair};
use pods_samples::{features_ack, handshake_ack, host};
use pods_sys::l2cap::{Channel, Received};

/// Longer than any loaded host takes to hand over a packet already queued.
pub(crate) const RECV_TIMEOUT: Duration = Duration::from_secs(5);
/// Ten times the fake's poll: a packet not here by then is not coming.
pub(crate) const QUIET: Duration = Duration::from_millis(200);
/// The slack a real time check allows past its bound.
pub(crate) const REAL_TIME_SLACK: Duration = Duration::from_millis(500);
/// Larger than any SDU the fake sends.
pub(crate) const BUFFER_LEN: usize = 65_536;
/// The burst after the handshake and features acknowledgements.
pub(crate) const BURST_LEN: usize = 36;

pub(crate) fn open(pods: &Pods) -> Channel {
    let (host, end) = pair();
    host.set_recv_timeout(RECV_TIMEOUT).unwrap();
    pods.serve(end);
    host
}

pub(crate) fn recv(host: &Channel) -> Vec<u8> {
    let mut buffer = vec![0; BUFFER_LEN];
    match host.recv(&mut buffer).unwrap() {
        Received::Sdu(len) => buffer[..len].to_vec(),
        other @ (Received::Truncated { .. } | Received::TimedOut | Received::Closed) => {
            panic!("wanted one SDU within {RECV_TIMEOUT:?}, got {other:?}")
        }
    }
}

pub(crate) fn recv_many(host: &Channel, count: usize) -> Vec<Vec<u8>> {
    (0..count).map(|_| recv(host)).collect()
}

pub(crate) fn assert_quiet(host: &Channel) {
    host.set_recv_timeout(QUIET).unwrap();
    let got = host.recv(&mut vec![0; BUFFER_LEN]).unwrap();
    host.set_recv_timeout(RECV_TIMEOUT).unwrap();
    assert_eq!(got, Received::TimedOut, "nothing more within {QUIET:?}");
}

pub(crate) fn ask(host: &Channel, frame: &[u8]) {
    host.send(frame).unwrap();
}

/// Runs the opening and returns the burst.
pub(crate) fn opening(host: &Channel) -> Vec<Vec<u8>> {
    ask(host, &host::handshake());
    assert_eq!(recv(host), handshake_ack());
    ask(host, &host::set_specific_features());
    assert_eq!(recv(host), features_ack());
    ask(host, &host::request_notifications());
    recv_many(host, BURST_LEN)
}
