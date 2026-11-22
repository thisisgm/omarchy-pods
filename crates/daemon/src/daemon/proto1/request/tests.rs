//! Which lines are requests, which refusals echo the id, and that no bytes panic the reader.

use proptest::prelude::*;

use super::super::MAX_LINE_BYTES;
use super::*;

/// Arrays in a request object at the deepest JSON `serde_json` reads, 127 levels with the object.
const ARRAYS_AT_THE_DEPTH_LIMIT: usize = 126;

/// Used often by the generated objects, so the checks past them run.
const MEMBERS: [&str; 6] = ["v", "id", "op", "key", "value", "action"];

fn bad(id: Option<u64>) -> Result<(u64, Request), Refusal> {
    Err(Refusal {
        id,
        error: RequestError::Command(CommandError::BadRequest),
    })
}

fn refused(id: u64, error: CommandError) -> Result<(u64, Request), Refusal> {
    Err(Refusal {
        id: Some(id),
        error: RequestError::Command(error),
    })
}

#[test]
fn each_op_reads_to_its_request() {
    let anc = settings::setting("noise", "anc").unwrap();
    for (line, expected) in [
        (r#"{"v":1,"id":7,"op":"get"}"#, Ok((7, Request::Get))),
        (
            r#"{"v":1,"id":8,"op":"subscribe"}"#,
            Ok((8, Request::Subscribe)),
        ),
        (
            r#"{"v":1,"id":9,"op":"set","key":"noise","value":"anc"}"#,
            Ok((9, Request::Submit(anc))),
        ),
        (
            r#"{"v":1,"id":10,"op":"do","action":"disconnect"}"#,
            Ok((10, Request::Submit(Command::Disconnect))),
        ),
        (
            r#" {"op":"get","id":0,"v":1,"x":[{}]} "#,
            Ok((0, Request::Get)),
        ),
        (r#"{"v":1,"id":4,"id":5,"op":"get"}"#, Ok((5, Request::Get))),
    ] {
        assert_eq!(parse(line.as_bytes()), expected, "{line}");
    }
}

#[test]
fn a_line_without_a_usable_id_is_refused_with_no_id() {
    for line in [
        "",
        "{",
        "null",
        "[1]",
        r#""get""#,
        r#"{"v":1,"op":"get"}"#,
        r#"{"v":2}"#,
        "{}",
        r#"{"v":1,"id":"4","op":"get"}"#,
        r#"{"v":1,"id":4.0,"op":"get"}"#,
        r#"{"v":1,"id":-4,"op":"get"}"#,
        r#"{"v":1,"id":18446744073709551616,"op":"get"}"#,
        r#"{"v":1,"id":null,"op":"get"}"#,
        r#"{"v":1,"id":4,"op":"get"} {}"#,
    ] {
        assert_eq!(parse(line.as_bytes()), bad(None), "{line}");
    }
    assert_eq!(parse(b"{\"v\":1,\"id\":4,\"op\":\"g\xffet\"}"), bad(None));
}

#[test]
fn a_line_with_a_usable_id_echoes_it_in_the_refusal() {
    let unsupported = Err(Refusal {
        id: Some(4),
        error: RequestError::UnsupportedVersion,
    });
    for (line, expected) in [
        (r#"{"id":4,"op":"get"}"#, bad(Some(4))),
        (r#"{"id":4}"#, bad(Some(4))),
        (r#"{"v":1,"id":4}"#, bad(Some(4))),
        (r#"{"v":1,"id":4,"op":null}"#, bad(Some(4))),
        (r#"{"v":2,"id":4}"#, unsupported),
        (r#"{"v":2,"id":4,"op":"get"}"#, unsupported),
        (r#"{"v":"1","id":4,"op":"get"}"#, unsupported),
        (r#"{"v":null,"id":4,"op":"get"}"#, unsupported),
        (r#"{"v":2,"id":4,"op":"fly"}"#, unsupported),
        (r#"{"v":1,"id":4,"op":"fly"}"#, bad(Some(4))),
        (r#"{"v":1,"id":4,"op":["get"]}"#, bad(Some(4))),
        (r#"{"v":1,"id":4,"op":"set","value":"anc"}"#, bad(Some(4))),
        (r#"{"v":1,"id":4,"op":"set","key":"noise"}"#, bad(Some(4))),
        (
            r#"{"v":1,"id":4,"op":"set","key":"adaptive_level","value":50}"#,
            bad(Some(4)),
        ),
        (
            r#"{"v":1,"id":4,"op":"set","key":"noise","value":"loud"}"#,
            refused(4, CommandError::BadValue),
        ),
        (
            r#"{"v":1,"id":4,"op":"set","key":"volume","value":"3"}"#,
            refused(4, CommandError::UnknownKey),
        ),
        (
            r#"{"v":1,"id":4,"op":"set","key":"adaptive_level","value":"101"}"#,
            refused(4, CommandError::OutOfRange),
        ),
        (r#"{"v":1,"id":4,"op":"do"}"#, bad(Some(4))),
        (r#"{"v":1,"id":4,"op":"do","action":true}"#, bad(Some(4))),
        (
            r#"{"v":1,"id":4,"op":"do","action":"reopen"}"#,
            bad(Some(4)),
        ),
        (
            r#"{"v":1,"id":18446744073709551615,"op":"fly"}"#,
            bad(Some(u64::MAX)),
        ),
    ] {
        assert_eq!(parse(line.as_bytes()), expected, "{line}");
    }
}

#[test]
fn nesting_at_the_json_depth_limit_is_read_and_one_level_more_is_refused() {
    let nested = |arrays: usize| {
        format!(
            r#"{{"v":1,"id":4,"op":"get","x":{}{}}}"#,
            "[".repeat(arrays),
            "]".repeat(arrays)
        )
    };
    let at_the_limit = nested(ARRAYS_AT_THE_DEPTH_LIMIT);
    assert_eq!(parse(at_the_limit.as_bytes()), Ok((4, Request::Get)));
    let past_the_limit = nested(ARRAYS_AT_THE_DEPTH_LIMIT + 1);
    assert_eq!(parse(past_the_limit.as_bytes()), bad(None));
}

fn any_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<f64>().prop_map(Value::from),
        Just(Value::from(VERSION)),
        prop_oneof![
            Just("get"),
            Just("subscribe"),
            Just("set"),
            Just("do"),
            Just("noise"),
            Just("anc"),
            Just("adaptive_level"),
            Just("101"),
            Just("disconnect"),
        ]
        .prop_map(Value::from),
        ".{0,12}".prop_map(Value::from),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            proptest::collection::btree_map(".{0,6}", inner, 0..4)
                .prop_map(|map| Value::Object(map.into_iter().collect())),
        ]
    })
}

fn any_request_object() -> impl Strategy<Value = Value> {
    proptest::collection::vec(proptest::option::of(any_value()), MEMBERS.len()).prop_map(|values| {
        let mut members = Map::new();
        for (name, value) in MEMBERS.iter().zip(values) {
            if let Some(value) = value {
                members.insert((*name).to_owned(), value);
            }
        }
        Value::Object(members)
    })
}

fn check_against_members(value: &Value) -> Result<(), TestCaseError> {
    let line = value.to_string();
    let id = value.get("id").and_then(Value::as_u64);
    match parse(line.as_bytes()) {
        Ok((echoed, request)) => {
            prop_assert_eq!(Some(echoed), id);
            prop_assert_eq!(value.get("v").and_then(Value::as_u64), Some(VERSION));
            let op = value.get("op").and_then(Value::as_str);
            let expected_op = match request {
                Request::Get => "get",
                Request::Subscribe => "subscribe",
                Request::Submit(_) => {
                    prop_assert!(matches!(op, Some("set" | "do")), "{}", line);
                    return Ok(());
                }
            };
            prop_assert_eq!(op, Some(expected_op));
        }
        Err(Refusal {
            id: Some(echoed),
            error,
        }) => {
            prop_assert_eq!(Some(echoed), id);
            let version = value.get("v");
            if error == RequestError::UnsupportedVersion {
                prop_assert!(
                    version.is_some_and(|v| v.as_u64() != Some(VERSION)),
                    "{}",
                    line
                );
            }
        }
        Err(Refusal { id: None, error }) => {
            prop_assert_eq!(error, RequestError::Command(CommandError::BadRequest));
            prop_assert_eq!(id, None, "{}", line);
        }
    }
    Ok(())
}

proptest! {
    #[test]
    fn any_bytes_parse_without_panic(bytes in proptest::collection::vec(any::<u8>(), 0..=MAX_LINE_BYTES)) {
        if let Err(refusal) = parse(&bytes) {
            prop_assert!(matches!(refusal.error, RequestError::Command(_) | RequestError::UnsupportedVersion));
        }
    }

    #[test]
    fn any_json_value_parses_without_panic(value in any_value()) {
        check_against_members(&value)?;
    }

    #[test]
    fn any_request_members_give_a_request_or_the_refusal_their_id_allows(value in any_request_object()) {
        check_against_members(&value)?;
    }

    #[test]
    fn any_bytes_after_a_request_start_parse_without_panic(
        tail in proptest::collection::vec(any::<u8>(), 0..64),
    ) {
        let line = [br#"{"v":1,"id":4,"op":"set","key":"noise","value":""#.as_slice(), &tail].concat();
        let result = parse(&line);
        prop_assert!(matches!(result, Ok((4, Request::Submit(_))) | Err(_)), "{:?}", result);
    }
}
