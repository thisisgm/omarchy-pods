//! Line framing at the bound, that no bytes can make it panic or keep more than one line, and the close after the client ends its side.

use proptest::prelude::*;
use tokio::io::AsyncReadExt;
use tokio::runtime::Builder;

use super::*;

/// A set the daemon refuses with no link, which still counts as a change.
const REFUSED_SET: &str = r#"{"v":1,"id":0,"op":"set","key":"noise","value":"anc"}"#;

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
fn the_protocol_document_states_the_limits_in_force() {
    let document = include_str!("../../../../../docs/protocol-1.md");
    let words = document.split_whitespace().collect::<Vec<_>>().join(" ");
    let first_byte = crate::daemon::legacy_dialect::REQUEST_DEADLINE;
    for deadline in [STALL_DEADLINE, first_byte] {
        assert_eq!(
            deadline.subsec_nanos(),
            0,
            "the document counts whole seconds"
        );
    }
    let stall = STALL_DEADLINE.as_secs();
    let clients = crate::daemon::socket::MAX_CONNECTIONS;
    // Each number in the wording the document gives it; `second` also matches `seconds`.
    for wanted in [
        format!("holds at most {MAX_LINE_BYTES} bytes"),
        format!("keeps at most {QUEUE_LINES} lines"),
        format!("When a {}th would wait", QUEUE_LINES + 1),
        format!("after {stall} second"),
        format!("The {stall} second"),
        format!("or {stall} second"),
        format!("within {} second", first_byte.as_secs()),
        format!("at most {clients} clients"),
    ] {
        assert!(
            words.contains(&wanted),
            "docs/protocol-1.md lacks `{wanted}`"
        );
    }
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

#[test]
fn a_client_that_ended_its_side_gets_every_line_and_change_it_is_owed() {
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    let received = runtime.block_on(async {
        let hub = Hub::new();
        let (served, mut client) = UnixStream::pair().unwrap();
        let (_, changes) = hub.follow();
        let Ok((_, Request::Submit(command))) = request::parse(REFUSED_SET.as_bytes()) else {
            panic!("the refused set does not parse as a set");
        };
        drop(hub.submit(command));
        let mut connection = Connection {
            stream: served,
            pending: format!("{REFUSED_SET}\n{REFUSED_SET}\n").into_bytes(),
            chunk: [0; PENDING_BYTES],
            queue: VecDeque::new(),
            written: 0,
            changes: Some(changes),
            ended: true,
            stall: None,
        };
        connection.run(&hub).await.unwrap();
        drop(connection);
        let mut received = String::new();
        client.read_to_string(&mut received).await.unwrap();
        received
    });
    let replies = received.matches(r#""ok":false"#).count();
    let events = received.matches(r#"{"event""#).count();
    assert_eq!((replies, events), (2, 3), "{received}");
}
