//! Support tests: pods that talk without end or never read, and an adapter that ended.

use super::*;

#[test]
fn a_flood_from_the_pods_leaves_the_runtime_its_turns() {
    on_runtime(async {
        let (connect, mut peer) = Peer::connector();
        let bench = Bench::start(connect);
        bench.ask(DictationRequest::Arm).await;
        assert_eq!(peer.read().await.unwrap(), host::declarations_from(1));
        let flood = peer.flood();
        assert!(flood >= FLOOD_MIN, "a socket full at {flood} packets");
        // This task's turns until the host answers a request sent behind the flood.
        let mut turns = 0_usize;
        let mut asked = false;
        let mut answer = [0_u8; ANSWER_BUFFER_BYTES];
        loop {
            turns = turns.saturating_add(1);
            let request = sample::request(MTU_REQUEST);
            asked = asked || peer.open.channel().try_send(&request).is_ok();
            let read = peer.open.channel().try_recv(&mut answer).unwrap();
            if let Received::Sdu(_) = read {
                break;
            }
            assert!(turns < flood.saturating_mul(REFILLS), "no answer");
            task::yield_now().await;
        }
        assert!(
            turns >= flood,
            "{turns} turns of this task while the adapter read {flood} packets"
        );
    });
}

#[test]
fn a_flood_does_not_keep_a_silent_arm_alive() {
    on_runtime(async {
        let _clock = hold_clock();
        let (connect, mut peer) = Peer::connector();
        let mut bench = Bench::start(connect);
        bench.ask(DictationRequest::Arm).await;
        assert_eq!(peer.read().await.unwrap(), host::declarations_from(1));
        let flood = peer.flooded().await;
        assert_eq!(bench.told_so_far(), None);
        time::advance(Duration::from_millis(ARM_ANSWER_MS)).await;
        let mut turns = 0_usize;
        let told = loop {
            if let Some(fact) = bench.told_so_far() {
                break fact;
            }
            turns = turns.saturating_add(1);
            assert!(turns < flood.saturating_mul(REFILLS), "the flood won");
            peer.flood();
            task::yield_now().await;
        };
        assert_eq!(told, DictationFact::BearerFailed);
    });
}

#[test]
fn a_flood_does_not_keep_a_request_waiting() {
    on_runtime(async {
        let _clock = hold_clock();
        let (connect, mut peer) = Peer::connector();
        let mut bench = Bench::start(connect);
        bench.ask(DictationRequest::Arm).await;
        assert_eq!(peer.read().await.unwrap(), host::declarations_from(1));
        let flood = peer.flooded().await;
        bench.ask(DictationRequest::Close).await;
        let mut turns = 0_usize;
        let mut unread = [0_u8; ANSWER_BUFFER_BYTES];
        loop {
            match peer.open.channel().try_recv(&mut unread) {
                // The socket reports a reset when the host closes with unread packets.
                Ok(Received::Closed) | Err(ChannelError::Recv(Errno::CONNRESET)) => break,
                Ok(Received::Sdu(_) | Received::Truncated { .. } | Received::TimedOut) => {}
                Err(error) => panic!("checking for the host's close failed: {error}"),
            }
            turns = turns.saturating_add(1);
            assert!(turns < flood.saturating_mul(REFILLS), "the flood won");
            peer.flood();
            task::yield_now().await;
        }
        assert_eq!(bench.told_so_far(), None);
    });
}

#[test]
fn pods_that_never_read_their_answers_lose_the_channel() {
    on_runtime(async {
        // The clock is held, so the answer time is not what ends the channel.
        let _clock = hold_clock();
        let (connect, mut peer) = Peer::connector();
        let mut bench = Bench::start(connect);
        bench.ask(DictationRequest::Arm).await;
        assert_eq!(peer.read().await.unwrap(), host::declarations_from(1));
        let request = sample::request(MTU_REQUEST);
        let mut turns = 0_usize;
        let failed = loop {
            if let Some(fact) = bench.told_so_far() {
                break fact;
            }
            turns = turns.saturating_add(1);
            assert!(
                turns < TURNS.saturating_mul(REFILLS),
                "the host still writes"
            );
            // A full socket is the host still reading; the next turn has room again.
            peer.open.channel().try_send(&request).ok();
            task::yield_now().await;
        };
        assert_eq!(failed, DictationFact::BearerFailed);
        while peer.read().await.is_some() {}
    });
}

/// An outlet whose adapter waits in the send of its next fact, behind a queue of one that is full.
async fn parked() -> (Outlet, mpsc::Sender<Input>, mpsc::Receiver<Input>) {
    let (connect, mut peer) = Peer::connector();
    let (inputs, facts) = mpsc::channel(1);
    inputs.try_send(Input::SendFailed).unwrap();
    let mut outlet = Outlet::new(connect);
    outlet.open(device(), inputs.clone(), Tap::new());
    outlet.offer(DictationRequest::Arm);
    assert_eq!(peer.read().await.unwrap(), host::declarations_from(1));
    drop(peer);
    // The runtime reads the channel's end while this task sleeps.
    sleep(POLL).await;
    (outlet, inputs, facts)
}

/// The queue gives up what filled it, and the parked adapter puts nothing behind that.
async fn told_nothing_more(mut facts: mpsc::Receiver<Input>) {
    assert!(matches!(facts.recv().await, Some(Input::SendFailed)));
    sleep(POLL).await;
    assert!(facts.try_recv().is_err(), "a fact of an ended adapter");
}

#[test]
fn a_parked_adapter_tells_bearer_failed_when_the_queue_drains() {
    on_runtime(async {
        let (_outlet, _inputs, mut facts) = parked().await;
        assert!(matches!(facts.recv().await, Some(Input::SendFailed)));
        // On this one thread, one yield runs only a sender already waiting on the slot just freed.
        task::yield_now().await;
        assert!(
            matches!(
                facts.try_recv(),
                Ok(Input::Dictation(_, DictationFact::BearerFailed))
            ),
            "the adapter was not parked in its send when parked() returned"
        );
    });
}

#[test]
fn a_closed_outlets_adapter_tells_nothing_more() {
    on_runtime(async {
        let (mut outlet, _inputs, facts) = parked().await;
        outlet.close();
        told_nothing_more(facts).await;
    });
}

#[test]
fn a_second_open_ends_the_adapter_before_it() {
    on_runtime(async {
        let (mut outlet, inputs, facts) = parked().await;
        outlet.open(device(), inputs, Tap::new());
        told_nothing_more(facts).await;
    });
}
