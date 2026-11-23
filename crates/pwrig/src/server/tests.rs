//! The reader of what `pw-dump` prints, on the output of the oldest `PipeWire` the workspace supports, and the wait for the server's socket.

use rustix::net::{self, AddressFamily, SocketAddrUnix, SocketType};
use serde_json::Value;

use super::{node_names, parse_dump, wait_accepting};

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

#[test]
fn a_socket_is_ready_only_once_it_listens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bound");
    let socket = net::socket(AddressFamily::UNIX, SocketType::STREAM, None).unwrap();
    net::bind(&socket, &SocketAddrUnix::new(&path).unwrap()).unwrap();
    let mut unready_polls = 0;
    let started = wait_accepting(&path, || {
        unready_polls += 1;
        net::listen(&socket, 1).unwrap();
        None
    });
    assert_eq!(started, Some(Ok(())));
    assert_eq!(unready_polls, 1, "ready before listen");
}
