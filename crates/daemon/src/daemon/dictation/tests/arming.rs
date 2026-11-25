//! Support tests: the arm, from the dial to its one answer, and the time each of its requests gets.

use super::*;

#[test]
fn arm_against_the_fake_gives_armed() {
    on_runtime(async {
        let served = Served::new();
        let mut bench = Bench::start(served.connector());
        bench.ask(DictationRequest::Arm).await;
        assert_eq!(bench.told().await, DictationFact::Armed);
    });
}

#[test]
fn a_table_without_the_service_fails_the_arm_and_the_next_arm_dials_again() {
    on_runtime(async {
        let served = Served::new();
        served.doap.table(Table::Absent);
        let mut bench = Bench::start(served.connector());
        bench.ask(DictationRequest::Arm).await;
        let no_service = DictationFact::ArmFailed(ArmFailure::NoService);
        assert_eq!(bench.told().await, no_service);
        served.doap.table(Table::Observed);
        bench.ask(DictationRequest::Arm).await;
        assert_eq!(bench.told().await, DictationFact::Armed);
    });
}

#[test]
fn a_failed_arm_drops_the_channel_at_once() {
    on_runtime(async {
        let _clock = hold_clock();
        let (connect, mut peer) = Peer::connector();
        let mut bench = Bench::start(connect);
        bench.ask(DictationRequest::Arm).await;
        assert_eq!(peer.read().await.unwrap(), host::declarations_from(1));
        let nothing = sample::error(sample::READ_BY_TYPE_REQUEST, 1, sample::ATTRIBUTE_NOT_FOUND);
        peer.send(&nothing);
        let no_service = DictationFact::ArmFailed(ArmFailure::NoService);
        assert_eq!(bench.told_now().await, no_service);
        peer.closed_now().await;
        assert_eq!(bench.told_so_far(), None);
    });
}

#[test]
fn a_refused_connect_gives_bearer_failed() {
    on_runtime(async {
        let mut bench = Bench::start(refusing());
        bench.ask(DictationRequest::Arm).await;
        assert_eq!(bench.told().await, DictationFact::BearerFailed);
    });
}

#[test]
fn a_pod_request_is_refused_while_the_arm_is_still_out() {
    on_runtime(async {
        let (connect, mut peer) = Peer::connector();
        let bench = Bench::start(connect);
        bench.ask(DictationRequest::Arm).await;
        assert_eq!(peer.read().await.unwrap(), host::declarations_from(1));
        peer.barrier().await;
        peer.send(&sample::UNASKED_INDICATION);
        assert_eq!(peer.read().await.unwrap(), host::CONFIRMED);
    });
}

#[test]
fn a_channel_that_ends_before_the_arm_is_done_is_bearer_failed() {
    on_runtime(async {
        let (connect, mut peer) = Peer::connector();
        let mut bench = Bench::start(connect);
        bench.ask(DictationRequest::Arm).await;
        assert_eq!(peer.read().await.unwrap(), host::declarations_from(1));
        drop(peer);
        assert_eq!(bench.told().await, DictationFact::BearerFailed);
    });
}

#[test]
fn the_requests_ending_ends_the_adapter_and_its_channel() {
    on_runtime(async {
        let (connect, mut peer) = Peer::connector();
        let Bench {
            requests, adapter, ..
        } = Bench::start(connect);
        requests
            .send((ArmId::default(), DictationRequest::Arm))
            .await
            .unwrap();
        assert_eq!(peer.read().await.unwrap(), host::declarations_from(1));
        drop(requests);
        timeout(HANG_LIMIT, adapter).await.unwrap().unwrap();
        assert_eq!(peer.read().await, None);
    });
}

#[test]
fn an_arm_the_pods_never_answer_fails_the_bearer_at_the_answer_time() {
    on_runtime(async {
        let _clock = hold_clock();
        let (connect, mut peer) = Peer::connector();
        let mut bench = Bench::start(connect);
        bench.ask(DictationRequest::Arm).await;
        assert_eq!(peer.read().await.unwrap(), host::declarations_from(1));
        time::advance(MOST_OF_IT).await;
        // A packet that answers nothing buys no time.
        peer.send(&UNASKED_NOTIFICATION);
        time::advance(THE_REST.saturating_sub(ONE_MS)).await;
        peer.barrier().await;
        assert_eq!(bench.told_so_far(), None);
        time::advance(PAST_THE_END).await;
        assert_eq!(bench.told_now().await, DictationFact::BearerFailed);
        assert_eq!(peer.read().await, None);
    });
}

#[test]
fn each_answer_of_an_arm_gets_the_whole_answer_time() {
    on_runtime(async {
        let _clock = hold_clock();
        let (connect, mut peer) = Peer::connector();
        let mut bench = Bench::start(connect);
        bench.ask(DictationRequest::Arm).await;
        assert_eq!(peer.read().await.unwrap(), host::declarations_from(1));
        time::advance(MOST_OF_IT).await;
        peer.send(&sample::declarations(&TABLE.declared()));
        let [.., last] = TABLE.declared();
        let next = last.at.checked_add(1).unwrap();
        assert_eq!(peer.read().await.unwrap(), host::declarations_from(next));
        time::advance(MOST_OF_IT).await;
        time::advance(THE_REST.saturating_sub(ONE_MS)).await;
        peer.barrier().await;
        assert_eq!(bench.told_so_far(), None);
        time::advance(PAST_THE_END).await;
        assert_eq!(bench.told_now().await, DictationFact::BearerFailed);
    });
}
