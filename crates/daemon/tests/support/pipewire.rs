//! The private `PipeWire` server a source suite starts in the session's runtime directory, where the daemon finds it, and the daemon's node as `pw-dump` shows it.

use std::time::{Duration, Instant};

use pods_fake::Profile;
use pods_pwrig::{Recorder, Server, node_names};
use pods_samples::MODEL_NUMBER;
use serde_json::Value;

use super::HANG_LIMIT;
use super::rig::{Device, Rig};
use super::uplink::{link, rig_with, wait_mic};

const LINK_TYPE: &str = "PipeWire:Interface:Link";

/// The server in the session's runtime directory, ended before the rig when the test holds it in a binding after the rig's.
pub(crate) fn start(rig: &Rig) -> Server {
    Server::start(&rig.session().runtime_dir())
}

/// The daemon on a confirmed model under `auto`, so only a recorder wants the uplink, and the server up before the link.
pub(crate) fn ready() -> (Rig, Device, Server) {
    let (rig, device) = rig_with("auto", &[MODEL_NUMBER], Profile::default());
    let pipewire = start(&rig);
    link(&rig, &device);
    wait_node(&pipewire);
    wait_mic(&rig, "ready");
    (rig, device, pipewire)
}

/// The node the daemon makes for the sample pods.
pub(crate) fn node_name() -> String {
    format!("omarchypods_input.{}", MODEL_NUMBER.to_lowercase())
}

pub(crate) fn wait_node(server: &Server) -> Value {
    server.wait_node(&node_name(), HANG_LIMIT)
}

pub(crate) fn wait_no_node(server: &Server) {
    server.wait_no_node(&node_name(), HANG_LIMIT);
}

/// Fails if the daemon's node shows at any point of `span` on `server`.
pub(crate) fn holds_no_node(server: &Server, span: Duration) {
    let started = Instant::now();
    while started.elapsed() < span {
        let names = node_names(&server.dump());
        assert!(
            !names.contains(&node_name()),
            "the node showed after {:?}: {names:?}",
            started.elapsed()
        );
    }
}

/// Superwhisper's recorder on the node, linked by hand as no session manager runs.
pub(crate) fn record<'server>(rig: &Rig, server: &'server Server, file: &str) -> Recorder<'server> {
    let path = rig.session().dir.path().join(file);
    let recorder = Recorder::superwhisper(server, &node_name(), &path);
    server.link(&node_name(), recorder.node());
    recorder
}

/// The links of `dump` with `node` at either end.
// Sample input: `{"id":61,"type":"PipeWire:Interface:Link","info":{"output-node-id":57,"input-node-id":59}}`.
pub(crate) fn links_of(dump: &Value, node: &Value) -> Vec<Value> {
    let id = node["id"].clone();
    let objects = dump.as_array().unwrap();
    objects
        .iter()
        .filter(|object| object["type"] == LINK_TYPE)
        .filter(|link| link["info"]["output-node-id"] == id || link["info"]["input-node-id"] == id)
        .cloned()
        .collect()
}
