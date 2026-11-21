//! The request rules: which lines are requests, which refusals echo the id, and that no bytes can panic the reader.

use proptest::prelude::*;

use super::*;

/// The longest request line, so the properties reach the daemon's bound.
const LONGEST_LINE: usize = 4096;

/// Members a request reads, which the generated objects use often so the checks past them run.
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
fn a_line_without_a_usable_id_v_or_op_is_refused_with_no_id() {
    for line in [
        "",
        "{",
        "null",
        "[1]",
        r#""get""#,
        r#"{"id":4,"op":"get"}"#,
        r#"{"v":1,"id":4}"#,
        r#"{"v":1,"op":"get"}"#,
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
            bad(Some(4)),
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
fn nesting_past_the_json_depth_limit_is_refused() {
    let deep = "[".repeat(LONGEST_LINE);
    assert_eq!(parse(deep.as_bytes()), bad(None));
    let object = format!(
        r#"{{"v":1,"id":4,"op":"get","x":{}}}"#,
        "[".repeat(LONGEST_LINE)
    );
    assert_eq!(parse(object.as_bytes()), bad(None));
}

/// Any JSON value, nested a few levels.
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

/// An object of request members, each present or not, with any value.
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

/// What a parse may return for `value`, checked against the members it was built from.
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
            id: Some(echoed), ..
        }) => {
            prop_assert_eq!(Some(echoed), id);
            prop_assert!(
                value.get("v").is_some() && value.get("op").is_some(),
                "{}",
                line
            );
        }
        Err(Refusal { id: None, error }) => {
            prop_assert_eq!(error, RequestError::Command(CommandError::BadRequest));
            let complete = id.is_some() && value.get("v").is_some() && value.get("op").is_some();
            prop_assert!(!complete, "{}", line);
        }
    }
    Ok(())
}

proptest! {
    #[test]
    fn any_bytes_parse_without_panic(bytes in proptest::collection::vec(any::<u8>(), 0..LONGEST_LINE)) {
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
