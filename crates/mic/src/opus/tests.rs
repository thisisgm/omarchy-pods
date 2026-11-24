//! Support test of the fence itself, with a stand-in for a decoder call that panics: a test cannot order a panic from the real decoder.

use pods_samples::opus::tone;

use super::*;

#[test]
fn a_decoder_that_panicked_is_reported_as_panic() {
    let mut decoder = Decoder::new().unwrap();
    let mut twin = Decoder::new().unwrap();
    let mut frames = tone();
    let first = frames.next().unwrap();
    assert_eq!(decoder.frame(first), twin.frame(first));
    let panicked = decoder.fenced(|_, _| panic!("the stand-in for the decoder panics"));
    assert_eq!(panicked, Decoded::Panic);
    // The stand-in touched nothing, so a decoder left as the panic left it goes on as its twin does.
    let second = frames.next().unwrap();
    let heard = decoder.frame(second);
    assert!(matches!(heard, Decoded::Samples(_)), "{heard:?}");
    assert_eq!(heard, twin.frame(second));
}
