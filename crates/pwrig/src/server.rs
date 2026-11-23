//! The private server: its own configuration and runtime directory, and the stock tools pointed at it.

use std::cell::RefCell;
use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use rustix::process::Signal;
use serde_json::{Value, json};

use crate::{Process, end, log_text, signal_and_wait, spawn, tool_output, wait_until};

/// The one node the rig's configuration makes: the driver that runs the graph with no device.
pub const DRIVER_NODE: &str = "Dummy-Driver";
/// Without a priority above zero the driver never ran the graph.
const DRIVER_PRIORITY: u32 = 1;
/// The graph cycle in frames, 42.7 ms at 48 kHz, which absorbs a client a loaded host holds back; 512 dropped audio.
const MIN_QUANTUM: u32 = 2048;

/// The name `core.name` gives the socket inside the runtime directory.
const SOCKET: &str = "pipewire-0";
const CONFIG_FILE: &str = "pipewire.conf";
const LOG_FILE: &str = "pipewire.log";
/// The configuration line of the module that makes the node of a client.
const CLIENT_NODE_MODULE: &str = "{ name = libpipewire-module-client-node }";

/// A local server opens its socket, and exits after SIGTERM, in milliseconds; this is a hung one.
const SERVER_WITHIN: Duration = Duration::from_secs(5);
/// How long a stock tool may run against the local server.
const TOOL_WITHIN: Duration = Duration::from_secs(5);
/// How long `link` waits for each node and for the ports its port configuration makes.
const LINK_WITHIN: Duration = Duration::from_secs(5);

/// The directories a child would read the user's configuration from, write its state to, or find a server in.
const PRIVATE_VARIABLES: [&str; 3] = ["XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME"];
/// The variables that would point a child at the session's audio server.
const SESSION_VARIABLES: [&str; 3] = ["PIPEWIRE_REMOTE", "PIPEWIRE_RUNTIME_DIR", "PULSE_SERVER"];
/// The addresses of the session's and the system's message bus, which the client library asks for real time priority.
const BUS_VARIABLES: [&str; 2] = ["DBUS_SESSION_BUS_ADDRESS", "DBUS_SYSTEM_BUS_ADDRESS"];
/// A socket name inside the private directory that nothing ever makes.
const NO_BUS: &str = "no-bus";

const NODE_TYPE: &str = "PipeWire:Interface:Node";
const NODE_NAME: &str = "/info/props/node.name";

/// The sample format of a port in DSP mode: one float channel a port.
const DSP_FORMAT: &str = "F32P";

/// A `pipewire` process serving only the directory it was started in.
#[derive(Debug)]
pub struct Server {
    dir: PathBuf,
    process: RefCell<Option<Child>>,
}

impl Server {
    /// Writes the configuration into `runtime_dir`, starts `pipewire` there and waits for its socket.
    #[must_use]
    pub fn start(runtime_dir: &Path) -> Self {
        Self::start_with(runtime_dir, CLIENT_NODE_MODULE)
    }

    /// As `start`, without the module that makes a client's node: a client connects and its node is refused, across `restart` too.
    #[must_use]
    pub fn start_without_client_nodes(runtime_dir: &Path) -> Self {
        Self::start_with(runtime_dir, "")
    }

    fn start_with(runtime_dir: &Path, client_node_module: &str) -> Self {
        let config = runtime_dir.join(CONFIG_FILE);
        fs::write(&config, config_text(client_node_module))
            .unwrap_or_else(|error| panic!("{}: {error}", config.display()));
        let server = Self {
            dir: runtime_dir.to_owned(),
            process: RefCell::new(None),
        };
        server.launch();
        server
    }

    pub fn socket(&self) -> PathBuf {
        self.dir.join(SOCKET)
    }

    /// A child for this server: its directories are the server's, and no variable names the session's server.
    pub fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        for (name, value) in environment(&self.dir) {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        command
    }

    /// Plays `file` with the stock player; with no session manager nothing links it but `link`.
    pub fn play(&self, file: &Path) -> Process<'_> {
        let mut command = self.command("pw-play");
        command.arg(file);
        Process::start(&mut command, Stdio::null(), file.with_extension("log"))
    }

    pub fn dump(&self) -> Value {
        let printed = self.run("pw-dump", &[]);
        parse_dump(&printed).unwrap_or_else(|error| panic!("pw-dump printed no JSON: {error}"))
    }

    pub fn wait_node(&self, name: &str, within: Duration) -> Value {
        wait_until(within, || node(&self.dump(), name)).unwrap_or_else(|| {
            panic!(
                "node `{name}` not seen within {within:?}; the server has {:?}",
                node_names(&self.dump())
            )
        })
    }

    pub fn wait_no_node(&self, name: &str, within: Duration) {
        let gone = wait_until(within, || node(&self.dump(), name).is_none().then_some(()));
        gone.unwrap_or_else(|| panic!("node `{name}` still present after {within:?}"));
    }

    /// Makes the ports of both nodes and links the two port by port.
    pub fn link(&self, from: &str, to: &str) {
        let outputs = self.ports(from, Direction::Output);
        let inputs = self.ports(to, Direction::Input);
        assert_eq!(
            outputs.len(),
            inputs.len(),
            "`{from}` has the ports {outputs:?} and `{to}` has {inputs:?}"
        );
        for (output, input) in outputs.iter().zip(&inputs) {
            self.run("pw-link", &[output, input]);
        }
    }

    /// Sends SIGTERM and waits for a clean exit, after which the server has removed its socket.
    pub fn stop(&self) {
        let mut child = self
            .process
            .take()
            .unwrap_or_else(|| panic!("the server in {} is not running", self.dir.display()));
        let status = signal_and_wait(&mut child, Signal::TERM, SERVER_WITHIN);
        assert!(
            status.success(),
            "pipewire ended with {status}, not by the stop; its log: {}",
            self.server_log()
        );
    }

    /// Stops the server and starts a new one in the same directory, with an empty graph.
    pub fn restart(&self) {
        self.stop();
        self.launch();
    }

    fn launch(&self) {
        assert!(
            !self.socket().exists(),
            "{} already exists, so its appearing cannot mean this server is ready",
            self.socket().display()
        );
        let log = self.dir.join(LOG_FILE);
        let errors = File::options()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap_or_else(|error| panic!("{}: {error}", log.display()));
        let mut command = self.command("pipewire");
        command
            .arg("-c")
            .arg(self.dir.join(CONFIG_FILE))
            .stdout(Stdio::null())
            .stderr(errors);
        // Kept before the wait, so a failed start still ends the process when the server drops.
        *self.process.borrow_mut() = Some(spawn(&mut command));
        let started = wait_until(SERVER_WITHIN, || {
            if self.socket().exists() {
                return Some(Ok(()));
            }
            let mut process = self.process.borrow_mut();
            let child = process.as_mut()?;
            let exited = child
                .try_wait()
                .unwrap_or_else(|error| panic!("waiting for pipewire: {error}"));
            exited.map(Err)
        });
        let failure = match started {
            Some(Ok(())) => return,
            Some(Err(status)) => format!("pipewire exited with {status}"),
            None => format!("pipewire opened no socket within {SERVER_WITHIN:?}"),
        };
        panic!("{failure}; its log: {}", self.server_log())
    }

    /// What the server wrote to its standard error, failing with the path and the reason when it cannot be read.
    pub fn log(&self) -> String {
        tool_output("pipewire", &self.dir.join(LOG_FILE))
    }

    /// The server's log for a failure message: its text, or the path and why it cannot be read.
    fn server_log(&self) -> String {
        log_text(&self.dir.join(LOG_FILE))
    }

    /// Where a stock tool's standard error goes; each run starts it afresh.
    fn tool_log(&self, program: &str) -> PathBuf {
        self.dir.join(format!("{program}.log"))
    }

    /// Runs a stock tool to its end and gives its standard output, failing on a failed exit or a hang.
    fn run(&self, program: &str, args: &[&str]) -> String {
        let printed = self.dir.join(format!("{program}.out"));
        let stdout =
            File::create(&printed).unwrap_or_else(|error| panic!("{}: {error}", printed.display()));
        let mut command = self.command(program);
        command.args(args);
        let mut process = Process::start(&mut command, stdout.into(), self.tool_log(program));
        let status = process
            .wait(TOOL_WITHIN)
            .unwrap_or_else(|| panic!("`{program} {args:?}` still running after {TOOL_WITHIN:?}"));
        assert!(
            status.success(),
            "`{program} {args:?}` failed with {status}: {}",
            process.log()
        );
        tool_output(program, &printed)
    }

    /// The ports of node `name` in `direction`, setting its port configuration when it has none yet.
    fn ports(&self, name: &str, direction: Direction) -> Vec<String> {
        let node = self.wait_node(name, LINK_WITHIN);
        let channels = channels(&node);
        // A node linked before keeps its ports, and pipewire refuses a second port configuration on it.
        if self.listed_ports(name, direction).len() != channels {
            self.configure_ports(&node, direction);
        }
        let ports = wait_until(LINK_WITHIN, || {
            let ports = self.listed_ports(name, direction);
            (ports.len() == channels).then_some(ports)
        });
        ports.unwrap_or_else(|| {
            panic!(
                "node `{name}` has not {channels} {direction:?} ports within {LINK_WITHIN:?}; pw-cli logged: {}",
                log_text(&self.tool_log("pw-cli"))
            )
        })
    }

    /// Puts `node` in DSP mode for `direction` with the channels of its own format.
    fn configure_ports(&self, node: &Value, direction: Direction) {
        let mut format = format(node).clone();
        format.insert("format".to_owned(), Value::from(DSP_FORMAT));
        let config = json!({"direction": direction.word(), "mode": "dsp", "format": format});
        let id = node
            .get("id")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("a node with no id: {node}"))
            .to_string();
        // pw-cli prints an error and still exits 0 when the node refuses, so the wait for the ports is the check.
        self.run(
            "pw-cli",
            &["set-param", &id, "PortConfig", &config.to_string()],
        );
    }

    fn listed_ports(&self, node: &str, direction: Direction) -> Vec<String> {
        let prefix = format!("{node}:");
        // Sample input: `pw-play:output_MONO`, one `node:port` a line.
        self.run("pw-link", &[direction.flag()])
            .lines()
            .filter(|line| line.starts_with(&prefix))
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(child) = self.process.get_mut() {
            end(child);
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Direction {
    Output,
    Input,
}

impl Direction {
    fn word(self) -> &'static str {
        match self {
            Self::Output => "Output",
            Self::Input => "Input",
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Self::Output => "-o",
            Self::Input => "-i",
        }
    }
}

/// What a process that loads the client library gets, value by value, and loses, to stay inside `dir`: no user configuration, no session server, no message bus.
#[must_use]
pub fn environment(dir: &Path) -> Vec<(&'static str, Option<OsString>)> {
    let mut no_bus = OsString::from("unix:path=");
    no_bus.push(dir.join(NO_BUS));
    let private = PRIVATE_VARIABLES.map(|name| (name, Some(dir.as_os_str().to_owned())));
    let session = SESSION_VARIABLES.map(|name| (name, None));
    let buses = BUS_VARIABLES.map(|name| (name, Some(no_bus.clone())));
    [&private[..], &session, &buses].concat()
}

/// The objects a `pw-dump` printed, in every document it printed: `PipeWire` 1.0 adds one, even without `--monitor`, for each object removed while it runs.
// Sample input: `[{"id":0,"type":"PipeWire:Interface:Core","info":{"props":{}}},{"id":45,"type":"PipeWire:Interface:Client","info":{"props":{}}}]\n[{"id":45,"info":null}]\n`.
pub fn parse_dump(printed: &str) -> Result<Value, String> {
    let mut objects: Vec<Value> = Vec::new();
    let mut read_any = false;
    for document in serde_json::Deserializer::from_str(printed).into_iter::<Vec<Value>>() {
        read_any = true;
        for object in document.map_err(|error| error.to_string())? {
            let id = object.get("id").cloned();
            objects.retain(|kept| kept.get("id") != id.as_ref());
            if !is_removal(&object) {
                objects.push(object);
            }
        }
    }
    if !read_any {
        return Err("the output holds no document".to_owned());
    }
    Ok(Value::Array(objects))
}

/// True for the entry `pw-dump` prints for a removed object, whose `info`, or `props` when it has no info, is null.
fn is_removal(object: &Value) -> bool {
    ["info", "props"]
        .iter()
        .any(|key| object.get(key).is_some_and(Value::is_null))
}

/// The `node.name` of every node in a `pw-dump`, sorted.
#[must_use]
pub fn node_names(dump: &Value) -> Vec<String> {
    let mut names: Vec<String> = nodes(dump)
        .filter_map(|node| node.pointer(NODE_NAME).and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    names.sort();
    names
}

fn node(dump: &Value, name: &str) -> Option<Value> {
    nodes(dump)
        .find(|node| node.pointer(NODE_NAME).and_then(Value::as_str) == Some(name))
        .cloned()
}

fn nodes(dump: &Value) -> impl Iterator<Item = &Value> {
    let objects = dump.as_array().map_or(&[][..], Vec::as_slice);
    objects
        .iter()
        .filter(|object| object.get("type").and_then(Value::as_str) == Some(NODE_TYPE))
}

/// The first format `node` offers.
// Sample input: `{"mediaType":"audio","mediaSubtype":"raw","format":"S16LE","rate":48000,"channels":1,"position":["MONO"]}`.
fn format(node: &Value) -> &serde_json::Map<String, Value> {
    node.pointer("/info/params/EnumFormat/0")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("the node offers no format: {node}"))
}

/// The channel count of the first format `node` offers, which is how many DSP ports it gets.
fn channels(node: &Value) -> usize {
    format(node)
        .get("channels")
        .and_then(Value::as_u64)
        .and_then(|channels| usize::try_from(channels).ok())
        .unwrap_or_else(|| panic!("the node's format has no channel count: {node}"))
}

/// The modules one by one: no ALSA, Bluetooth or session manager module is loaded.
fn config_text(client_node_module: &str) -> String {
    format!(
        "context.properties = {{
    core.daemon = true
    core.name = {SOCKET}
    # The server has no real-time priority, so a long cycle keeps a late client from missing its turn.
    default.clock.min-quantum = {MIN_QUANTUM}
}}
context.spa-libs = {{
    audio.convert.* = audioconvert/libspa-audioconvert
    support.* = support/libspa-support
}}
context.modules = [
    {{ name = libpipewire-module-protocol-native }}
    # Grants each client its permissions; without it every client waits for ever.
    {{ name = libpipewire-module-access }}
    # Serves the settings object every client binds; without it the bind is a protocol error.
    {{ name = libpipewire-module-metadata }}
    {client_node_module}
    {{ name = libpipewire-module-adapter }}
    {{ name = libpipewire-module-link-factory }}
    {{ name = libpipewire-module-spa-node-factory }}
]
context.objects = [
    {{ factory = spa-node-factory
        args = {{
            factory.name = support.node.driver
            node.name = {DRIVER_NODE}
            priority.driver = {DRIVER_PRIORITY}
        }}
    }}
]
"
    )
}

#[cfg(test)]
mod tests;
