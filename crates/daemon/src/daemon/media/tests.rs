//! Support tests: which status replies read as playing, whatever a player puts in them, and the players asked at once on a paused clock.

use std::future::pending;
use std::time::Duration;

use tokio::time::Instant;
use zbus::{Message, Variant};

use super::{PLAYER_CALL_TIMEOUT_MS, PLAYER_PATH, PLAYING, ask_each, reads_playing, within};
use crate::daemon::paused::on_paused_clock;

/// A reply to a `Get` call, carrying `body`.
fn reply(body: &(impl zbus::export::serde::Serialize + zbus::DynamicType)) -> Message {
    let call = Message::method_call(PLAYER_PATH, "Get").build(&()).unwrap();
    Message::method_return(&call.header()).build(body).unwrap()
}

#[test]
fn only_the_string_playing_in_a_variant_reads_as_playing() {
    assert!(reads_playing(&reply(&Variant::from(PLAYING))));
    let long = "Playing".repeat(10_000);
    let refused = [
        reply(&Variant::from("Paused")),
        reply(&Variant::from("playing")),
        reply(&Variant::from("Playing ")),
        reply(&Variant::from(long.as_str())),
        reply(&Variant::from(1_u32)),
        reply(&Variant::Variant(Box::new(Variant::from(PLAYING)))),
        reply(&()),
    ];
    for refused in refused {
        assert!(!reads_playing(&refused), "{refused:?}");
    }
}

#[test]
fn a_player_that_never_answers_delays_no_other_player() {
    let answers = on_paused_clock(async {
        let started = Instant::now();
        // Listed first, so asking the players in turn would wait out its timeout before the other.
        let ask = |hangs: bool| async move {
            if hangs {
                within(pending::<()>()).await;
            }
            Some((hangs, started.elapsed()))
        };
        ask_each(vec![true, false], ask).await
    });
    let timeout = Duration::from_millis(PLAYER_CALL_TIMEOUT_MS);
    assert_eq!(answers, [(false, Duration::ZERO), (true, timeout)]);
}
