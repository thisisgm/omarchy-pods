//! The built daemon under the trap, a private system bus with the fake BlueZ, a private session bus and the fake pods, joined.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::mem;
use std::ops::RangeInclusive;
use std::path::Path;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pods_fake::{Pods, Profile};
use pods_proto::addr::BdAddr;
use pods_proto::packets::CONTROL_PSM;
use pods_trap::{Answer, Dial, Route, Trap, TrapError};
use rustix::process::{Signal, kill_process};
use serde_json::Value;

use super::bluez::{AAP_UUID, AdapterPath, Bluez, Call, DeviceMethod, DevicePath, FakeBluez};
use super::bus::{Bus, bus_address, start_bus};
use super::{
    BINARY, DAEMON_LOG, Daemon, HANG_LIMIT, Proto1, Session, do_request, outcome, wait_for,
    wait_within,
};

pub(crate) const REAL_TIME_SLACK: Duration = Duration::from_millis(500);
/// A dial may leave just as `Connected` falls.
pub(crate) const SIGNAL_SLACK: Duration = Duration::from_millis(200);
/// The most a reply or a status may take while a BlueZ call hangs.
pub(crate) const ANSWERED_WITHIN: Duration = Duration::from_millis(200);
/// BlueZ method calls the daemon leaves unanswered at once before it fails the next.
pub(crate) const CALLS_IN_FLIGHT_MAX: usize = 4;
/// A dial that should come at once.
pub(crate) const DIAL_WAIT: Duration = Duration::from_secs(1);
/// How long the daemon waits for the pods to report a setting back.
pub(crate) const CONFIRM_TIMEOUT: Duration = Duration::from_millis(700);
/// The wait after each failed dial to pods BlueZ holds connected, the C++ `delayMs(1..=10)` without its jitter.
pub(crate) const HELD_LADDER: [Duration; 10] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(16),
    Duration::from_secs(16),
    Duration::from_secs(16),
    Duration::from_secs(16),
    Duration::from_secs(16),
];

/// A listening mode as the pods carry it on the wire and as the status document numbers it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Mode {
    pub(crate) wire: u8,
    pub(crate) status: u8,
}

pub(crate) const ANC: Mode = Mode { wire: 2, status: 1 };
pub(crate) const TRANSPARENCY: Mode = Mode { wire: 3, status: 2 };
pub(crate) const ADAPTIVE: Mode = Mode { wire: 4, status: 3 };

const SYSTEM_BUS: &str = "system";
const SESSION_BUS: &str = "session";
const WATCH_LOG_NAME: &str = "watch.log";
/// Every fixture address is this prefix and one more octet; the daemon's log may hold neither form.
const ADDRESS_PREFIX: [u8; 5] = [0x02, 0x00, 0x00, 0x00, 0x00];
const LOGGED_ADDRESS: [&str; 2] = ["02:00:00", "02_00_00"];

#[derive(Clone)]
pub(crate) struct Device {
    pub(crate) address: BdAddr,
    pub(crate) path: DevicePath,
    pub(crate) pods: Pods,
}

impl Device {
    /// The call `method` on this device's own object, as the fake BlueZ records it.
    pub(crate) fn call(&self, method: DeviceMethod) -> Call {
        Call {
            object: self.path.0.clone(),
            method,
            argument: None,
        }
    }
}

type Prepare = Box<dyn FnOnce(&mut Command)>;

/// Declared in the order they must end: the daemon before the trap, the fake BlueZ before its bus.
pub(crate) struct Rig {
    daemon: Option<Daemon>,
    /// Set once the test itself has ended the daemon, the one exit before `finish` that is no failure.
    stopped: bool,
    trap: Option<Trap>,
    bluez: Option<FakeBluez>,
    adapter: Option<AdapterPath>,
    system_bus: Option<Bus>,
    session_bus: Bus,
    /// Shared with the fake BlueZ's reaction to calls.
    devices: Arc<Mutex<Vec<Device>>>,
    prepare: Vec<Prepare>,
    session: Session,
}

/// The fixture address ending in `last`.
pub(crate) fn test_address(last: u8) -> BdAddr {
    let [o0, o1, o2, o3, o4] = ADDRESS_PREFIX;
    BdAddr::from([o0, o1, o2, o3, o4, last])
}

impl Rig {
    /// A session, both private buses, the fake BlueZ owning its name and one adapter; no daemon yet.
    pub(crate) fn new() -> Self {
        let mut rig = Self::before_system_bus();
        rig.bring_up_bluez(true);
        rig
    }

    /// As `new`, but nobody owns `org.bluez` until `bluez().take_name()`.
    pub(crate) fn before_bluez() -> Self {
        let mut rig = Self::before_system_bus();
        rig.bring_up_bluez(false);
        rig
    }

    /// As `new`, but no system bus runs until `start_system_bus`.
    pub(crate) fn before_system_bus() -> Self {
        let session = Session::new();
        let session_bus = start_bus(&session, SESSION_BUS);
        Self {
            daemon: None,
            stopped: false,
            trap: None,
            bluez: None,
            adapter: None,
            system_bus: None,
            session_bus,
            devices: Arc::default(),
            prepare: Vec::new(),
            session,
        }
    }

    pub(crate) fn start_system_bus(&mut self) {
        self.bring_up_bluez(true);
    }

    fn bring_up_bluez(&mut self, own_name: bool) {
        let bus = start_bus(&self.session, SYSTEM_BUS);
        let mut bluez = FakeBluez::start(&bus.address, own_name);
        let devices = Arc::clone(&self.devices);
        bluez.on_call(move |bluez, call| react(bluez, call, &devices));
        self.adapter = Some(bluez.add_adapter());
        self.bluez = Some(bluez);
        self.system_bus = Some(bus);
    }

    pub(crate) fn system_bus_address(&self) -> String {
        bus_address(&self.session, SYSTEM_BUS)
    }

    pub(crate) fn session(&self) -> &Session {
        &self.session
    }

    pub(crate) fn bluez(&self) -> &FakeBluez {
        self.bluez.as_ref().expect("a running system bus")
    }

    pub(crate) fn adapter(&self) -> &AdapterPath {
        self.adapter.as_ref().expect("a running system bus")
    }

    /// Paired AirPods, not connected, at the next fixture address, routed to fake pods of their own.
    pub(crate) fn add_pods(&mut self, profile: Profile) -> Device {
        let last = u8::try_from(lock(&self.devices).len())
            .unwrap()
            .saturating_add(1);
        let address = test_address(last);
        let pods = Pods::new(profile);
        let path = self
            .bluez()
            .add_device(self.adapter(), address, &[AAP_UUID], false);
        if let Some(trap) = &self.trap {
            trap.route(address, CONTROL_PSM, serve(&pods, Duration::ZERO));
        }
        let device = Device {
            address,
            path,
            pods,
        };
        lock(&self.devices).push(device.clone());
        device
    }

    /// The daemon with one pair of pods it knows, paired and not connected.
    pub(crate) fn with_known_pods() -> (Self, Device) {
        let mut rig = Self::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        rig.wait_disconnect_answers("not_connected");
        (rig, device)
    }

    /// Asks `do disconnect` until it answers `code`, which tells what the daemon knows without a BlueZ call.
    pub(crate) fn wait_disconnect_answers(&self, code: &str) {
        let mut client = Proto1::connect(self.session());
        wait_for(&format!("do disconnect to answer {code}"), || {
            (outcome(&client.ask(&do_request(1, "disconnect"))) == code).then_some(())
        });
    }

    /// Changes the daemon's command before a start.
    pub(crate) fn prepare(&mut self, change: impl FnOnce(&mut Command) + 'static) {
        self.prepare.push(Box::new(change));
    }

    /// Starts the built daemon under the trap and returns once its socket accepts.
    pub(crate) fn start(&mut self) {
        let routes = lock(&self.devices)
            .iter()
            .map(|device| Route {
                device: device.address,
                psm: CONTROL_PSM,
                answer: serve(&device.pods, Duration::ZERO),
            })
            .collect();
        let command = self.command();
        let (child, trap) =
            pods_trap::spawn(command, routes).unwrap_or_else(|error| panic!("{error}"));
        self.daemon = Some(self.guard(child));
        self.trap = Some(trap);
        self.session.wait_accepting();
    }

    /// Starts the built daemon with no trap; only a machine without Bluetooth may run this.
    pub(crate) fn start_untrapped(&mut self) {
        let child = self.command().spawn().unwrap();
        self.daemon = Some(self.guard(child));
        self.session.wait_accepting();
    }

    fn command(&mut self) -> Command {
        // Appended to, so a line a test adds is never written over by the daemon's next one.
        let log = File::options()
            .create(true)
            .append(true)
            .open(self.session.dir.path().join(DAEMON_LOG))
            .unwrap();
        let mut command = self.session.command(Path::new(BINARY), &["daemon"]);
        command
            .env(
                "DBUS_SYSTEM_BUS_ADDRESS",
                bus_address(&self.session, SYSTEM_BUS),
            )
            .env("DBUS_SESSION_BUS_ADDRESS", &self.session_bus.address)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        for change in self.prepare.drain(..) {
            change(&mut command);
        }
        command
    }

    fn guard(&self, child: std::process::Child) -> Daemon {
        Daemon {
            child,
            log: self.session.dir.path().join(DAEMON_LOG),
        }
    }

    /// BlueZ reports the device connected.
    pub(crate) fn link_up(&self, device: &Device) {
        link_up(self.bluez(), &device.path);
    }

    /// BlueZ reports the device gone and its pods hang up, as a lost link does.
    pub(crate) fn link_down(&self, device: &Device) {
        link_down(self.bluez(), &device.path, &device.pods);
    }

    /// The address of the session bus the daemon serves its D-Bus interface on.
    pub(crate) fn session_bus(&self) -> &str {
        &self.session_bus.address
    }

    /// Replaces the device's route, for the next dials.
    pub(crate) fn answer(&self, device: &Device, answer: Answer) {
        self.trap().route(device.address, CONTROL_PSM, answer);
    }

    /// Routes the device back to its fake pods.
    pub(crate) fn accept(&self, device: &Device) {
        self.trap().route(
            device.address,
            CONTROL_PSM,
            serve(&device.pods, Duration::ZERO),
        );
    }

    pub(crate) fn dials(&self) -> Vec<Dial> {
        self.trap().dials()
    }

    /// The Bluetooth socket calls the trap saw.
    pub(crate) fn sockets(&self) -> usize {
        self.trap().sockets()
    }

    /// Waits for `count` dials in all, failing with the whole record at the deadline.
    pub(crate) fn wait_dials(&self, count: usize, deadline: Duration) -> Vec<Dial> {
        self.trap()
            .wait_dials(count, deadline)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// Fails with the record if a dial comes within `quiet`.
    pub(crate) fn no_dial_for(&self, quiet: Duration) {
        let before = self.dials().len();
        match self.trap().wait_dials(before.saturating_add(1), quiet) {
            Err(TrapError::Deadline { .. }) => {}
            Err(error) => panic!("{error}"),
            Ok(dials) => {
                let record: Vec<String> = dials.iter().map(ToString::to_string).collect();
                panic!("a dial came within {quiet:?}: {record:#?}");
            }
        }
    }

    /// The `librepods` client in this session, run to its end.
    pub(crate) fn client(&self, args: &[&str]) -> Output {
        self.session.run_client(Path::new(BINARY), args)
    }

    /// `librepods status`, parsed.
    pub(crate) fn status(&self) -> Value {
        let output = self.client(&["status"]);
        assert!(output.status.success(), "librepods status: {output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// `librepods watch` in this session, its lines read as they come.
    pub(crate) fn watch(&self) -> Watch {
        let log = File::create(self.session.dir.path().join(WATCH_LOG_NAME)).unwrap();
        let mut child = self
            .session
            .command(Path::new(BINARY), &["watch"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let process = Daemon {
            child,
            log: self.session.dir.path().join(WATCH_LOG_NAME),
        };
        let (sender, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if sender.send((Instant::now(), line)).is_err() {
                    return;
                }
            }
        });
        Watch {
            process,
            lines,
            reader: Some(reader),
        }
    }

    /// Asks `librepods status` until `ready` holds, failing at `HANG_LIMIT`.
    pub(crate) fn wait_status(&self, what: &str, ready: impl Fn(&Value) -> bool) -> Value {
        wait_for(what, || {
            let status = self.status();
            ready(&status).then_some(status)
        })
    }

    /// Waits for the link up and the pods' name in the status, which the burst brings after the batteries and the ears.
    pub(crate) fn wait_filled(&self) -> Value {
        self.wait_status("the link up and the pods' name", |status| {
            connected(status) && named(status)
        })
    }

    pub(crate) fn daemon(&mut self) -> &mut Daemon {
        self.daemon.as_mut().expect("a started daemon")
    }

    fn trap(&self) -> &Trap {
        self.trap.as_ref().expect("a daemon started under the trap")
    }

    /// Sends SIGTERM as a test's own step and gives the exit status, which must come within `limit`.
    pub(crate) fn stop(&mut self, limit: Duration) -> ExitStatus {
        let daemon = self.daemon();
        kill_process(daemon.pid(), Signal::TERM).unwrap();
        let status = wait_within("the daemon to exit", limit, || {
            daemon.child.try_wait().unwrap()
        });
        self.stopped = true;
        status
    }

    /// Stops the daemon unless the test did, then fails the test unless every check of the rig holds.
    pub(crate) fn finish(self) {
        let Self {
            daemon,
            stopped,
            trap,
            bluez,
            devices,
            ..
        } = self;
        if let Some(mut daemon) = daemon {
            if !stopped {
                if let Some(status) = daemon.child.try_wait().unwrap() {
                    panic!(
                        "the daemon exited with {status} before the end of the test: {}",
                        daemon.log()
                    );
                }
                kill_process(daemon.pid(), Signal::TERM).unwrap();
                let status = daemon.wait_exit();
                assert!(
                    status.success(),
                    "the daemon exited with {status}: {}",
                    daemon.log()
                );
            }
            let log = daemon.log();
            for form in LOGGED_ADDRESS {
                assert!(
                    !log.contains(form),
                    "the daemon logged a device address:\n{log}"
                );
            }
        }
        if let Some(trap) = trap {
            let dials = trap.finish().unwrap_or_else(|error| panic!("{error}"));
            for dial in &dials {
                let reported = bluez
                    .as_ref()
                    .is_some_and(|bluez| bluez.connected_at(dial.device, dial.at, SIGNAL_SLACK));
                assert!(
                    reported,
                    "a dial to a device BlueZ did not report connected: {dial}"
                );
            }
        }
        if let Some(bluez) = bluez {
            bluez.finish();
        }
        let devices = mem::take(&mut *lock(&devices));
        for device in devices {
            device
                .pods
                .finish()
                .unwrap_or_else(|failure| panic!("{failure}"));
        }
    }
}

fn lock(devices: &Mutex<Vec<Device>>) -> MutexGuard<'_, Vec<Device>> {
    devices.lock().unwrap_or_else(PoisonError::into_inner)
}

fn link_up(bluez: &Bluez, path: &DevicePath) {
    bluez.set(path, "Connected", zbus::OwnedVariant::from(true));
}

fn link_down(bluez: &Bluez, path: &DevicePath, pods: &Pods) {
    bluez.set(path, "Connected", zbus::OwnedVariant::from(false));
    pods.hang_up();
}

/// Moves the fake BlueZ and the fake pods together after a call BlueZ answered, as the real ones move.
fn react(bluez: &Bluez, call: &Call, devices: &Mutex<Vec<Device>>) {
    let path = match call.method {
        DeviceMethod::Connect | DeviceMethod::Disconnect => call.object.clone(),
        DeviceMethod::RemoveDevice => call.argument.clone().expect("the device path"),
    };
    let device = lock(devices)
        .iter()
        .find(|device| device.path.0 == path)
        .cloned()
        .expect("a call on a device of the rig");
    match call.method {
        DeviceMethod::Connect => link_up(bluez, &device.path),
        DeviceMethod::Disconnect => link_down(bluez, &device.path, &device.pods),
        DeviceMethod::RemoveDevice => {
            bluez.remove(&device.path);
            device.pods.hang_up();
        }
    }
}

/// An answer that hands each dial to `pods`, `after` it was made.
pub(crate) fn serve(pods: &Pods, after: Duration) -> Answer {
    let pods = pods.clone();
    Answer::Accept {
        after,
        serve: Arc::new(move |channel| pods.serve(channel)),
    }
}

/// One line `librepods watch` printed, and when the test's reader took it.
#[derive(Debug)]
pub(crate) struct Printed {
    pub(crate) at: Instant,
    pub(crate) state: Value,
}

/// `librepods watch` in a guard, its output read on a thread the guard joins.
pub(crate) struct Watch {
    process: Daemon,
    lines: Receiver<(Instant, String)>,
    reader: Option<JoinHandle<()>>,
}

impl Watch {
    /// The next line, or none at the deadline, a watch that ended failing the test; sample input `{"adaptive_noise_level":50,"case":{"available":true,"charging":false,"level":50},"connected":true,...}`.
    pub(crate) fn next(&self, deadline: Duration) -> Option<Printed> {
        let (at, line) = match self.lines.recv_timeout(deadline) {
            Ok(printed) => printed,
            Err(RecvTimeoutError::Timeout) => return None,
            Err(RecvTimeoutError::Disconnected) => {
                panic!("watch ended: {}", self.process.log())
            }
        };
        let state = serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("watch printed {line:?}: {error}"));
        Some(Printed { at, state })
    }

    /// Every line up to and including the first equal to `last`, failing once `HANG_LIMIT` passes in all.
    pub(crate) fn lines_until(&self, last: &Value) -> Vec<Value> {
        let started = Instant::now();
        let mut lines = Vec::new();
        while lines.last() != Some(last) {
            let left = HANG_LIMIT.saturating_sub(started.elapsed());
            let Some(printed) = self.next(left) else {
                panic!("watch never printed {last}; it printed {lines:#?}");
            };
            lines.push(printed.state);
        }
        lines
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // A watch that already exited refuses the kill, which is the outcome wanted.
        drop(self.process.child.kill());
        drop(self.process.child.wait());
        if let Some(reader) = self.reader.take() {
            // The kill closed the pipe, so the reader is at its end; one that panicked has nothing left to clean up.
            drop(reader.join());
        }
    }
}

pub(crate) fn connected(status: &Value) -> bool {
    status["connected"].as_bool().unwrap()
}

/// The status carries a device name, which a missing key does not.
pub(crate) fn named(status: &Value) -> bool {
    status["device_name"]
        .as_str()
        .is_some_and(|name| !name.is_empty())
}

/// One `*_total` counter of a parsed status.
pub(crate) fn total(status: &Value, counter: &str) -> u64 {
    status[counter].as_u64().unwrap()
}

/// From `wanted` to `wanted` plus `REAL_TIME_SLACK`, the range a real time check takes.
pub(crate) fn not_before(wanted: Duration) -> RangeInclusive<Duration> {
    wanted..=wanted.saturating_add(REAL_TIME_SLACK)
}

/// Fails unless `to` came within `range` after `from`, and prints the gap it saw either way.
#[expect(
    clippy::print_stderr,
    reason = "the gap a time check saw goes into the test's output"
)]
pub(crate) fn assert_gap(what: &str, from: Instant, to: Instant, range: &RangeInclusive<Duration>) {
    let gap = to.checked_duration_since(from);
    eprintln!("{what}: {gap:?}, wanted {range:?}");
    assert!(
        gap.is_some_and(|gap| range.contains(&gap)),
        "{what}: {gap:?}, wanted {range:?}"
    );
}
