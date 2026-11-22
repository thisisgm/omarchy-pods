//! Support tests: which status replies read as playing, whatever a player puts in them.

use zbus::{Message, Variant};

use super::{PLAYER_PATH, PLAYING, reads_playing};

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
