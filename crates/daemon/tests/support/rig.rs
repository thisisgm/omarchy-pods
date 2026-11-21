//! The built daemon under the trap, a private system bus with the fake BlueZ, a private session bus and the fake pods, joined.

use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use pods_fake::{Pods, Profile};
use pods_proto::addr::BdAddr;
use pods_proto::packets::CONTROL_PSM;
use pods_trap::{Answer, Dial, Route, Trap, TrapError};
use rustix::process::{Signal, kill_process};
use serde_json::Value;

use super::bluez::{AAP_UUID, AdapterPath, DevicePath, FakeBluez};
use super::bus::{Bus, bus_address, start_bus};
use super::{BINARY, DAEMON_LOG, Daemon, Session, wait_for};

pub(crate) const REAL_TIME_SLACK: Duration = Duration::from_millis(500);
/// A dial may leave just as `Connected` falls.
pub(crate) const SIGNAL_SLACK: Duration = Duration::from_millis(200);
/// A dial that should come at once.
pub(crate) const DIAL_WAIT: Duration = Duration::from_secs(1);

const SYSTEM_BUS: &str = "system";
const SESSION_BUS: &str = "session";
/// Every fixture address is this prefix and one more octet; the daemon's log may hold neither form.
const ADDRESS_PREFIX: [u8; 5] = [0x02, 0x00, 0x00, 0x00, 0x00];
const LOGGED_ADDRESS: [&str; 2] = ["02:00:00", "02_00_00"];

pub(crate) struct Device {
    pub(crate) address: BdAddr,
    pub(crate) path: DevicePath,
    pub(crate) pods: Pods,
}

type Prepare = Box<dyn FnOnce(&mut Command)>;

/// Declared in the order they must end: the daemon before the trap, the fake BlueZ before its bus.
pub(crate) struct Rig {
    daemon: Option<Daemon>,
    trap: Option<Trap>,
    bluez: Option<FakeBluez>,
    adapter: Option<AdapterPath>,
    system_bus: Option<Bus>,
    session_bus: Bus,
    pods: Vec<(BdAddr, Pods)>,
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
            trap: None,
            bluez: None,
            adapter: None,
            system_bus: None,
            session_bus,
            pods: Vec::new(),
            prepare: Vec::new(),
            session,
        }
    }

    pub(crate) fn start_system_bus(&mut self) {
        self.bring_up_bluez(true);
    }

    fn bring_up_bluez(&mut self, own_name: bool) {
        let bus = start_bus(&self.session, SYSTEM_BUS);
        let bluez = FakeBluez::start(&bus.address, own_name);
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
        let last = u8::try_from(self.pods.len()).unwrap().saturating_add(1);
        let address = test_address(last);
        let pods = Pods::new(profile);
        let path = self
            .bluez()
            .add_device(self.adapter(), address, &[AAP_UUID], false);
        if let Some(trap) = &self.trap {
            trap.route(address, CONTROL_PSM, serve(&pods));
        }
        self.pods.push((address, pods.clone()));
        Device {
            address,
            path,
            pods,
        }
    }

    /// Changes the daemon's command before a start.
    pub(crate) fn prepare(&mut self, change: impl FnOnce(&mut Command) + 'static) {
        self.prepare.push(Box::new(change));
    }

    /// Starts the built daemon under the trap and returns once its socket accepts.
    pub(crate) fn start(&mut self) {
        let routes = self
            .pods
            .iter()
            .map(|(address, pods)| Route {
                device: *address,
                psm: CONTROL_PSM,
                answer: serve(pods),
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
        self.bluez()
            .set(&device.path, "Connected", zbus::OwnedVariant::from(true));
    }

    /// Replaces the device's route, for the next dials.
    pub(crate) fn answer(&self, device: &Device, answer: Answer) {
        self.trap().route(device.address, CONTROL_PSM, answer);
    }

    /// Routes the device back to its fake pods.
    pub(crate) fn accept(&self, device: &Device) {
        self.trap()
            .route(device.address, CONTROL_PSM, serve(&device.pods));
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

    /// `librepods status`, parsed.
    pub(crate) fn status(&self) -> Value {
        let output = self.session.run_client(Path::new(BINARY), &["status"]);
        assert!(output.status.success(), "librepods status: {output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// Asks `librepods status` until `ready` holds, failing at `HANG_LIMIT`.
    pub(crate) fn wait_status(&self, what: &str, ready: impl Fn(&Value) -> bool) -> Value {
        wait_for(what, || {
            let status = self.status();
            ready(&status).then_some(status)
        })
    }

    pub(crate) fn daemon(&mut self) -> &mut Daemon {
        self.daemon.as_mut().expect("a started daemon")
    }

    fn trap(&self) -> &Trap {
        self.trap.as_ref().expect("a daemon started under the trap")
    }

    /// Stops the daemon, then fails the test unless every check of the rig holds.
    pub(crate) fn finish(self) {
        let Self {
            daemon,
            trap,
            bluez,
            pods,
            ..
        } = self;
        if let Some(mut daemon) = daemon {
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
        for (_, pods) in pods {
            pods.finish().unwrap_or_else(|failure| panic!("{failure}"));
        }
    }
}

fn serve(pods: &Pods) -> Answer {
    let pods = pods.clone();
    Answer::Accept {
        after: Duration::ZERO,
        serve: Arc::new(move |channel| pods.serve(channel)),
    }
}
