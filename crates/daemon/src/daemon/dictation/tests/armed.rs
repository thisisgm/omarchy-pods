//! Support tests: the service on an armed channel, from the start to the channel's end.

use super::*;

#[test]
fn a_table_at_other_handles_is_armed_and_started_by_what_the_arm_found() {
    on_runtime(async {
        let served = Served::new();
        served.doap.table(Table::Shifted);
        served.mode().await;
        let mut bench = Bench::start(served.connector());
        bench.arm().await;
        bench.ask(DictationRequest::Start).await;
        assert_eq!(bench.told().await, DictationFact::Answered);
        assert_eq!(bench.told().await, DictationFact::FirstAudio);
    });
}

#[test]
fn a_pod_request_is_refused_and_an_indication_confirmed() {
    on_runtime(async {
        let served = Served::new();
        served.mode().await;
        let mut bench = Bench::start(served.connector());
        bench.arm().await;
        bench.ask(DictationRequest::Start).await;
        assert_eq!(bench.told().await, DictationFact::Answered);
        // While the stream runs: the answers do not wait for it.
        served.doap.send_request(MTU_REQUEST);
        served.doap.indicate();
        wait_for("one refusal and one confirmation", || {
            served.doap.answered() == (1, 1)
        })
        .await;
        assert!(served.doap.streaming());
    });
}

#[test]
fn start_sends_start_alone_and_arms_the_tap() {
    on_runtime(async {
        let (connect, mut peer) = Peer::connector();
        let mut bench = Bench::start(connect);
        bench.ask(DictationRequest::Arm).await;
        arm_by_hand(&mut peer).await;
        assert_eq!(bench.told().await, DictationFact::Armed);
        assert!(!tap_waits_for_audio(&bench.tap));
        bench.ask(DictationRequest::Start).await;
        assert_eq!(peer.read().await.unwrap(), host::start(&TABLE));
        // The refusal answers the barrier's request, so the start was the only packet before it.
        peer.barrier().await;
        assert!(tap_waits_for_audio(&bench.tap));
    });
}

#[test]
fn audio_reaches_the_tap_and_never_the_driver() {
    on_runtime(async {
        let served = Served::new();
        served.mode().await;
        let mut bench = Bench::start(served.connector());
        bench.arm().await;
        bench.ask(DictationRequest::Start).await;
        wait_for("audio at the tap", || {
            bench.tap.dictation_packets() >= HEARD
        })
        .await;
        bench
            .ask(DictationRequest::Stop {
                reason: STOP_REASON_HOST,
            })
            .await;
        let mut told = Vec::new();
        while told.last() != Some(&DictationFact::Ended) {
            told.push(bench.told().await);
        }
        let stream = [
            DictationFact::Answered,
            DictationFact::FirstAudio,
            DictationFact::Ended,
        ];
        assert_eq!(told, stream);
    });
}

#[test]
fn the_score_is_answered_and_the_end_is_ended() {
    on_runtime(async {
        let served = Served::new();
        served.mode().await;
        let mut bench = Bench::start(served.connector());
        bench.arm().await;
        bench.ask(DictationRequest::Start).await;
        assert_eq!(bench.told().await, DictationFact::Answered);
        assert_eq!(bench.told().await, DictationFact::FirstAudio);
        bench
            .ask(DictationRequest::Stop {
                reason: STOP_REASON_HOST,
            })
            .await;
        assert_eq!(bench.told().await, DictationFact::Ended);
        assert_eq!(served.doap.stops(), [STOP_REASON_HOST]);
    });
}

#[test]
fn only_the_score_of_a_start_the_host_asked_for_is_answered() {
    assert_eq!(told(Event::STARTED), DictationFact::Answered);
    assert_eq!(told(OTHER_SCORE), DictationFact::PodEvent(OTHER_SCORE));
    assert_eq!(told(Event::StreamingEnd), DictationFact::Ended);
    let cancel = DictationFact::PodEvent(Event::SiriCancel);
    assert_eq!(told(Event::SiriCancel), cancel);
}

#[test]
fn a_refusal_is_refused_with_its_reason() {
    on_runtime(async {
        let served = Served::new();
        // No mode was set, so the pods refuse the start.
        let mut bench = Bench::start(served.connector());
        bench.arm().await;
        bench.ask(DictationRequest::Start).await;
        let refused = DictationFact::Refused {
            reason: STOP_REASON_UNSOLICITED,
        };
        assert_eq!(bench.told().await, refused);
    });
}

#[test]
fn the_channel_ending_is_bearer_lost_once() {
    on_runtime(async {
        let served = Served::new();
        let mut bench = Bench::start(served.connector());
        bench.arm().await;
        served.pods.hang_up();
        assert_eq!(bench.told().await, DictationFact::BearerLost);
        // Requests for the lost channel tell nothing more, so the next fact is the new arm's.
        bench.ask(DictationRequest::Start).await;
        bench
            .ask(DictationRequest::Stop {
                reason: STOP_REASON_HOST,
            })
            .await;
        bench.arm().await;
        served.doap.send_request(MTU_REQUEST);
        wait_for("one refusal", || served.doap.answered() == (1, 0)).await;
        assert_eq!(served.doap.starts(), 0);
        assert!(!tap_waits_for_audio(&bench.tap));
    });
}

#[test]
fn a_close_drops_the_channel_and_tells_nothing() {
    on_runtime(async {
        let served = Served::new();
        served.mode().await;
        let mut bench = Bench::start(served.connector());
        bench.arm().await;
        bench.ask(DictationRequest::Start).await;
        assert_eq!(bench.told().await, DictationFact::Answered);
        assert_eq!(bench.told().await, DictationFact::FirstAudio);
        bench.ask(DictationRequest::Close).await;
        wait_for("the stream ended by the channel's end", || {
            !served.doap.streaming()
        })
        .await;
        // Nothing was told of the close, so the next fact is the new arm's.
        bench.arm().await;
    });
}

#[test]
fn an_arm_over_an_armed_channel_starts_over() {
    on_runtime(async {
        let served = Served::new();
        served.mode().await;
        let mut bench = Bench::start(served.connector());
        bench.arm().await;
        bench.ask(DictationRequest::Start).await;
        assert_eq!(bench.told().await, DictationFact::Answered);
        assert_eq!(bench.told().await, DictationFact::FirstAudio);
        bench.arm().await;
        wait_for("the first channel's stream ended", || {
            !served.doap.streaming()
        })
        .await;
        assert_eq!(bench.told_so_far(), None);
    });
}
