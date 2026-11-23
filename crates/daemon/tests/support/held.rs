//! A dial the trap holds open until the test lets it be answered.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use pods_fake::Pods;
use pods_trap::Answer;

/// Answers a dial with the pods only once the returned end is dropped.
pub(crate) fn held(pods: &Pods) -> (Answer, UnixStream) {
    let (release, held) = UnixStream::pair().unwrap();
    let pods = pods.clone();
    let answer = Answer::Held {
        release: Arc::new(OwnedFd::from(held)),
        serve: Arc::new(move |channel| pods.serve(channel)),
    };
    (answer, release)
}
