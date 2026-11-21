//! Line framing at the bound, and that no bytes can make it panic or keep more than one line.

use proptest::prelude::*;

use super::*;

#[test]
fn a_line_is_taken_without_its_newline_and_the_rest_waits() {
    let mut pending = b"{\"a\":1}\n{\"b\":".to_vec();
    assert_eq!(take_line(&mut pending), Ok(Some(b"{\"a\":1}".to_vec())));
    assert_eq!(pending, b"{\"b\":");
    assert_eq!(take_line(&mut pending), Ok(None));
    pending.extend_from_slice(b"2}\n\n");
    assert_eq!(take_line(&mut pending), Ok(Some(b"{\"b\":2}".to_vec())));
    assert_eq!(take_line(&mut pending), Ok(Some(Vec::new())));
    assert_eq!(pending, b"");
}

#[test]
fn a_line_at_the_bound_is_taken_and_one_byte_more_is_refused() {
    let mut pending = vec![b'x'; MAX_LINE_BYTES];
    assert_eq!(take_line(&mut pending), Ok(None));
    pending.push(b'\n');
    assert_eq!(
        take_line(&mut pending),
        Ok(Some(vec![b'x'; MAX_LINE_BYTES]))
    );
    let mut pending = vec![b'x'; MAX_LINE_BYTES + 1];
    assert!(take_line(&mut pending).is_err());
    pending.push(b'\n');
    assert!(take_line(&mut pending).is_err());
}

proptest! {
    #[test]
    fn any_bytes_give_bounded_lines_or_a_refusal(
        bytes in proptest::collection::vec(prop_oneof![Just(b'\n'), any::<u8>()], 0..2 * PENDING_BYTES),
    ) {
        let mut pending = bytes.clone();
        let mut taken = Vec::new();
        loop {
            match take_line(&mut pending) {
                Ok(Some(line)) => {
                    prop_assert!(line.len() <= MAX_LINE_BYTES);
                    prop_assert!(!line.contains(&b'\n'));
                    taken.extend_from_slice(&line);
                    taken.push(b'\n');
                }
                Ok(None) => {
                    prop_assert!(pending.len() <= MAX_LINE_BYTES);
                    taken.extend_from_slice(&pending);
                    prop_assert_eq!(&taken, &bytes);
                    break;
                }
                Err(_) => {
                    let line_end = pending.iter().position(|&byte| byte == b'\n').unwrap_or(pending.len());
                    prop_assert!(line_end > MAX_LINE_BYTES);
                    break;
                }
            }
        }
    }
}
