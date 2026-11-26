//! Support tests: the keys file's text and its parser.

use pods_samples::{IDENTITY_KEY, PAYLOAD_KEY};
use proptest::prelude::*;

use super::*;

/// The length of one pair's file: braces, two names, two quoted keys of 32 digits, a comma and a newline.
const FILE_LEN: usize = 93;

const IDENTITY_HEX: &str = "5ac30f9621e87b44d019a63e82f56cb7";

const PAYLOAD_HEX: &str = "9d0471ea38cf568b12fe67a02db943c8";

fn sample() -> SavedKeys {
    SavedKeys {
        keys: ProximityKeys {
            identity: Key::from_bytes(IDENTITY_KEY),
            payload: Key::from_bytes(PAYLOAD_KEY),
        },
        device: None,
    }
}

/// The identity key's digits without the first `count`.
fn cut_first(count: usize) -> String {
    IDENTITY_HEX.chars().skip(count).collect()
}

fn file(identity: &str, payload: &str) -> String {
    format!("{{\"identity\":\"{identity}\",\"payload\":\"{payload}\"}}\n")
}

#[test]
fn the_text_is_one_line_of_hex_within_its_buffer() {
    let text = render(&sample());
    assert_eq!(*text, file(IDENTITY_HEX, PAYLOAD_HEX));
    assert_eq!(text.len(), FILE_LEN);
    assert_eq!(text.capacity(), TEXT_CAPACITY);
}

#[test]
fn the_text_reads_back_and_either_case_is_taken() {
    assert_eq!(parse(render(&sample()).as_bytes()), Ok(sample()));
    let upper = file(&IDENTITY_HEX.to_uppercase(), &PAYLOAD_HEX.to_uppercase());
    assert_eq!(parse(upper.as_bytes()), Ok(sample()));
}

#[test]
fn a_cut_file_is_refused() {
    let whole = file(IDENTITY_HEX, PAYLOAD_HEX);
    for len in 0..whole.len() - 1 {
        let cut = &whole.as_bytes()[..len];
        assert_eq!(parse(cut), Err(FileError::NotAnObject), "{len}");
    }
}

#[test]
fn an_oversized_short_or_wrong_key_is_refused() {
    let wrong_identities = [
        format!("{IDENTITY_HEX}0"),
        cut_first(1),
        format!("+{}", cut_first(1)),
        format!("g{}", cut_first(1)),
        format!("\u{e9}{}", cut_first(2)),
    ];
    for identity in wrong_identities {
        let text = file(&identity, PAYLOAD_HEX);
        assert_eq!(parse(text.as_bytes()), Err(FileError::Identity), "{text}");
    }
    let text = file(IDENTITY_HEX, PAYLOAD_HEX.strip_suffix("c8").unwrap());
    assert_eq!(parse(text.as_bytes()), Err(FileError::Payload), "{text}");
    let numbers = format!("{{\"identity\":1,\"payload\":\"{PAYLOAD_HEX}\"}}");
    assert_eq!(parse(numbers.as_bytes()), Err(FileError::Identity));
    let missing = format!("{{\"identity\":\"{IDENTITY_HEX}\"}}");
    assert_eq!(parse(missing.as_bytes()), Err(FileError::Payload));
    for other in ["[]", "\"text\"", "null", ""] {
        assert_eq!(
            parse(other.as_bytes()),
            Err(FileError::NotAnObject),
            "{other}"
        );
    }
}

#[test]
fn an_unreadable_device_keeps_the_keys() {
    for device in ["\"zz\"", "\"0a1b2c3d4e\"", "1"] {
        let text = format!(
            "{{\"identity\":\"{IDENTITY_HEX}\",\"payload\":\"{PAYLOAD_HEX}\",\"device\":{device}}}\n"
        );
        assert_eq!(parse(text.as_bytes()), Ok(sample()), "{text}");
    }
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        drop(parse(&bytes));
    }

    #[test]
    fn any_pair_reads_back(identity in any::<[u8; KEY_LEN]>(), payload in any::<[u8; KEY_LEN]>(), device in any::<[u8; ADDRESS_LEN]>()) {
        let keys = SavedKeys {
            keys: ProximityKeys {
                identity: Key::from_bytes(identity),
                payload: Key::from_bytes(payload),
            },
            device: Some(BdAddr::from(device)),
        };
        let text = render(&keys);
        prop_assert_eq!(text.capacity(), TEXT_CAPACITY);
        prop_assert_eq!(parse(text.as_bytes()), Ok(keys));
    }
}
