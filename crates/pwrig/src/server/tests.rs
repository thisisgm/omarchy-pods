//! The reader of what `pw-dump` prints, on the output of the oldest `PipeWire` the workspace supports.

use serde_json::Value;

use super::{node_names, parse_dump};

/// A whole dump: the core, the rig's driver and one client.
const FULL: &str = r#"[
  {"id":0,"type":"PipeWire:Interface:Core","info":{"props":{}}},
  {"id":30,"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"Dummy-Driver"}}},
  {"id":45,"type":"PipeWire:Interface:Client","info":{"props":{}}}
]
"#;
/// What `PipeWire` 1.0 prints, even without `--monitor`, for a client that leaves while the dump runs.
const CLIENT_LEFT: &str = "[\n  {\n    \"id\": 45,\n    \"info\": null\n  }\n]\n";
/// The same for an object with no info of its own, such as a factory.
const FACTORY_LEFT: &str = "[{\"id\":7,\"props\":null}]\n";

fn ids(dump: &Value) -> Vec<u64> {
    let objects = dump.as_array().unwrap();
    objects
        .iter()
        .map(|object| object["id"].as_u64().unwrap())
        .collect()
}

#[test]
fn a_whole_dump_reads_as_its_objects() {
    let dump = parse_dump(FULL).unwrap();
    assert_eq!(ids(&dump), [0, 30, 45]);
    assert_eq!(node_names(&dump), ["Dummy-Driver"]);
}

#[test]
fn an_object_removed_after_the_dump_is_left_out() {
    let dump = parse_dump(&format!("{FULL}{CLIENT_LEFT}")).unwrap();
    assert_eq!(ids(&dump), [0, 30]);
    assert_eq!(node_names(&dump), ["Dummy-Driver"]);
}

#[test]
fn a_removal_before_the_dump_takes_out_nothing_the_dump_holds() {
    let dump = parse_dump(&format!("{FACTORY_LEFT}{FULL}")).unwrap();
    assert_eq!(ids(&dump), [0, 30, 45]);
}

#[test]
fn text_that_is_not_json_is_an_error() {
    assert!(parse_dump("").is_err());
    assert!(parse_dump("\n").is_err());
    assert!(parse_dump("not json").is_err());
    assert!(parse_dump(&format!("{FULL}]")).is_err());
}
