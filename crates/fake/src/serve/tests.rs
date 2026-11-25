use std::sync::atomic::{AtomicBool, Ordering};

use pods_samples::doap::{CODECS, Handles, REASON_HOST, host};

use super::*;

const CONTROL: Side = Side::Control(Fault::None);

#[test]
fn a_thread_that_panicked_fails_the_finish() {
    let pods = Pods::new(Profile::default());
    let (host, end) = crate::pair();
    pods.serve(end);
    pods.shared.lock().start(7, || panic!("on purpose"));
    let failure = pods.finish().unwrap_err();
    assert_eq!(failure, "fake-pods thread 7 panicked: on purpose");
    drop(host);
}

#[test]
fn a_host_that_sends_past_the_heard_limit_fails_the_finish() {
    const JUNK: [u8; 1] = [0xee];
    const DEADLINE: Duration = Duration::from_secs(5);
    let pods = Pods::new(Profile::default());
    let (host, end) = crate::pair();
    pods.serve(end);
    for _ in 0..HEARD_LIMIT {
        host.send(&JUNK).unwrap();
    }
    assert_eq!(
        pods.wait_heard(&JUNK, HEARD_LIMIT, DEADLINE).unwrap().len(),
        HEARD_LIMIT
    );
    assert_eq!(pods.shared.lock().failures, Vec::<String>::new());
    host.send(&JUNK).unwrap();
    pods.wait_closed(1, DEADLINE).unwrap();
    assert_eq!(pods.heard().len(), HEARD_LIMIT);
    assert_eq!(pods.finish().unwrap_err(), Overflow::Heard.message());
    drop(host);
}

#[test]
fn a_host_that_sends_past_the_heard_limit_on_an_attribute_channel_fails_the_finish() {
    const DEADLINE: Duration = Duration::from_secs(5);
    let table = Handles::OBSERVED;
    let stop = host::stop(&table, REASON_HOST);
    let pods = Pods::new(Profile::default());
    let (host, end) = crate::pair();
    host.set_recv_timeout(DEADLINE).unwrap();
    pods.serve_att(end);
    // One packet short of the limit in stops, then a read whose answer shows the pods heard them all.
    for _ in 1..HEARD_LIMIT {
        host.send(&stop).unwrap();
    }
    host.send(&host::read(table.supported_codecs)).unwrap();
    let mut answer = [0; CODECS.len()];
    assert_eq!(host.recv(&mut answer), Ok(Received::Sdu(CODECS.len())));
    assert_eq!(answer, CODECS);
    assert_eq!(pods.shared.lock().failures, Vec::<String>::new());
    host.send(&stop).unwrap();
    let hung_up = |state: &State| state.lines.is_empty().then_some(());
    pods.shared.wait_until(DEADLINE, hung_up).unwrap();
    assert_eq!(pods.doap().stops().len(), HEARD_LIMIT - 1);
    assert_eq!(pods.finish().unwrap_err(), Overflow::AttHeard.message());
    drop(host);
}

/// Pods with one channel no thread drains, its outbox at `OUTBOX_LIMIT` and nothing failed yet.
fn pods_with_a_full_outbox() -> Pods {
    let pods = Pods::new(Profile::default());
    let unread = Line {
        id: 0,
        side: CONTROL,
        outbox: VecDeque::new(),
        hang_up: false,
        deaf: false,
    };
    pods.shared.lock().lines.push(unread);
    for _ in 0..OUTBOX_LIMIT {
        pods.send(&[0xee]);
    }
    assert_eq!(pods.shared.lock().failures, Vec::<String>::new());
    pods
}

fn assert_hung_up_on_the_outbox_limit(pods: Pods) {
    let state = pods.shared.lock();
    assert_eq!(state.lines[0].outbox.len(), OUTBOX_LIMIT);
    assert!(state.lines[0].hang_up);
    drop(state);
    assert_eq!(pods.finish().unwrap_err(), Overflow::Outbox.message());
}

#[test]
fn wait_sent_returns_once_the_host_can_read_the_change() {
    const DEADLINE: Duration = Duration::from_secs(5);
    let pods = Pods::new(Profile::default());
    let (host, end) = crate::pair();
    pods.serve(end);
    pods.wait_sent(DEADLINE).unwrap();
    let mut buffer = vec![0; RECV_BUFFER_LEN];
    while matches!(host.try_recv(&mut buffer), Ok(Received::Sdu(_))) {}
    pods.set_ear(Placed::InEar, Placed::OutOfEar);
    pods.wait_sent(DEADLINE).unwrap();
    assert!(matches!(host.try_recv(&mut buffer), Ok(Received::Sdu(_))));
    pods.finish().unwrap();
    drop(host);
}

#[test]
fn wait_sent_fails_at_the_deadline_while_a_packet_is_queued() {
    let pods = pods_with_a_full_outbox();
    let waited = pods.wait_sent(Duration::from_millis(50)).unwrap_err();
    assert!(waited.waited >= Duration::from_millis(50), "{waited:?}");
}

#[test]
fn a_waiter_sees_an_outbox_drained_without_a_notify_within_a_poll() {
    const DEADLINE: Duration = Duration::from_secs(10);
    const WAKE_SLACK: Duration = Duration::from_secs(1);
    let pods = pods_with_a_full_outbox();
    let checked = AtomicBool::new(false);
    let started = Instant::now();
    thread::scope(|scope| {
        scope.spawn(|| {
            while !checked.load(Ordering::Acquire) {
                thread::yield_now();
            }
            // The waiter holds the lock until it sleeps, so this drain lands while it waits.
            pods.shared.lock().lines[0].outbox.clear();
        });
        let seen = pods.shared.wait_until(DEADLINE, |state| {
            checked.store(true, Ordering::Release);
            state.lines[0].outbox.is_empty().then_some(())
        });
        assert_eq!(seen, Ok(()));
    });
    // Not after one poll plus the slack a loaded runner takes to wake a thread.
    assert!(
        started.elapsed() < POLL + WAKE_SLACK,
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_change_past_the_outbox_limit_fails_the_finish() {
    let pods = pods_with_a_full_outbox();
    pods.set_setting(pods_samples::NOISE, 2);
    assert_hung_up_on_the_outbox_limit(pods);
}

#[test]
fn a_reply_past_the_outbox_limit_fails_the_finish() {
    let pods = pods_with_a_full_outbox();
    pods.shared
        .on_host(0, CONTROL, &pods_samples::host::handshake());
    assert_hung_up_on_the_outbox_limit(pods);
}

#[test]
fn a_send_to_a_host_that_closed_ends_the_channel_quietly() {
    for unread in [false, true] {
        let pods = pods_with_a_full_outbox();
        let (host, end) = crate::pair();
        if unread {
            end.send(&[0x01]).unwrap();
        }
        drop(host);
        let mut sent = 0;
        assert_eq!(
            pods.shared.send_queued(0, CONTROL, &end, &mut sent),
            Ok(false),
            "unread: {unread}"
        );
    }
}

#[test]
fn an_open_channel_is_not_counted_closed() {
    /// Five polls: a thread that ends by itself has ended by then.
    const OPEN_FOR: Duration = Duration::from_millis(100);
    let pods = Pods::new(Profile::default());
    let (host, end) = crate::pair();
    pods.serve(end);
    let waited = pods.wait_closed(1, OPEN_FOR);
    assert!(
        matches!(waited, Err(Deadline { waited }) if waited >= OPEN_FOR),
        "{waited:?}"
    );
    drop(host);
    pods.finish().unwrap();
}

#[test]
fn an_attribute_channel_is_no_control_channel_to_what_a_test_counts() {
    const DEADLINE: Duration = Duration::from_secs(5);
    let pods = Pods::new(Profile::default());
    let (host, end) = crate::pair();
    host.set_recv_timeout(DEADLINE).unwrap();
    pods.serve_att(end);
    // A change is reported on control channels only, so the first packet here is the answer to the read.
    pods.set_setting(pods_samples::NOISE, 2);
    let codecs = Handles::OBSERVED.supported_codecs;
    host.send(&host::read(codecs)).unwrap();
    let mut buffer = vec![0; RECV_BUFFER_LEN];
    let answer = CODECS;
    assert_eq!(host.recv(&mut buffer), Ok(Received::Sdu(answer.len())));
    assert_eq!(buffer[..answer.len()], answer);
    assert_eq!(pods.heard().len(), 0);
    assert_eq!(pods.most_open_at_once(), 0);
    drop(host);
    let ended = |state: &State| state.lines.is_empty().then_some(());
    pods.shared.wait_until(DEADLINE, ended).unwrap();
    assert_eq!(pods.shared.lock().closed.len(), 0);
    pods.finish().unwrap();
}
