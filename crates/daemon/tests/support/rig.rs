//! The built daemon under the trap, a private system bus with the fake BlueZ, a private session bus and the fake pods, joined.

use std::fmt::Debug;
use std::fs::File;
use std::mem;
use std::ops::{RangeBounds, RangeInclusive};
use std::path::Path;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use pods_fake::{Pods, Profile};
use pods_proto::addr::BdAddr;
use pods_proto::packets::CONTROL_PSM;
use pods_trap::{Answer, Dial, Route, Trap, TrapError};
use rustix::io::Errno;
use rustix::process::{Signal, kill_process};
use serde_json::Value;

use super::audio::wire::sink_named;
use super::audio::{Asked as AudioAsked, FakeAudio, PERCENT};
use super::bluez::{AAP_UUID, AdapterPath, Bluez, Call, DeviceMethod, DevicePath, FakeBluez};
use super::bus::{Bus, bus_address, bus_socket, start_bus};
use super::logind::{FakeLogind, Impostor};
use super::omarchy::{Mood, Omarchy};
use super::players::{FakePlayer, Playback};
use super::relay::Relay;
use super::watch::Watch;
use super::{
    DAEMON_LOG, Daemon, Proto1, Session, binary, do_request, outcome, wait_for, wait_within,
};

pub(crate) const REAL_TIME_SLACK: Duration = Duration::from_millis(500);
/// A sink no test names, added and removed to make the audio adapter refresh without changing the pods' sink.
const PASSING_SINK: &str = "alsa_output.passing";
/// A dial may leave just as `Connected` falls.
pub(crate) const SIGNAL_SLACK: Duration = Duration::from_millis(200);
/// BlueZ method calls the daemon leaves unanswered at once before it fails the next.
pub(crate) const CALLS_IN_FLIGHT_MAX: usize = 4;
/// A dial that should come at once.
pub(crate) const DIAL_WAIT: Duration = Duration::from_secs(1);
/// The C++ `firstDelayMs`: the wait before dialling again a link that was up.
pub(crate) const FIRST_RETRY: Duration = Duration::from_millis(750);
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

/// The C++ absent ladder as one wait, `firstDelayMs` then `delayMs(1..=3)`: 750 + 1000 + 2000 + 4000 ms.
pub(crate) const ABSENT_GRACE: Duration = Duration::from_millis(7750);

/// A listening mode as the pods carry it on the wire and as the status document numbers it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Mode {
    pub(crate) wire: u8,
    pub(crate) status: u8,
}

pub(crate) const OFF: Mode = Mode { wire: 1, status: 0 };
pub(crate) const ANC: Mode = Mode { wire: 2, status: 1 };
pub(crate) const TRANSPARENCY: Mode = Mode { wire: 3, status: 2 };
pub(crate) const ADAPTIVE: Mode = Mode { wire: 4, status: 3 };

const SYSTEM_BUS: &str = "system";
const SESSION_BUS: &str = "session";
/// The socket in the session the daemon takes for its system bus once a test relays it.
const SYSTEM_RELAY_SOCKET: &str = "system-relay.sock";
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

/// Declared in the order they must end: the daemon before the trap, the fakes before their bus.
pub(crate) struct Rig {
    daemon: Option<Daemon>,
    /// Set once the test itself has ended the daemon, the one exit before `finish` that is no failure.
    stopped: bool,
    trap: Option<Trap>,
    /// Between the daemon and the system bus once a test asks; the fakes reach the bus directly.
    relay: Option<Relay>,
    audio: Option<Rc<FakeAudio>>,
    bluez: Option<FakeBluez>,
    adapter: Option<AdapterPath>,
    logind: Option<FakeLogind>,
    system_bus: Option<Bus>,
    session_bus: Bus,
    /// Shared with the fake BlueZ's reaction to calls.
    devices: Arc<Mutex<Vec<Device>>>,
    prepare: Vec<Prepare>,
    /// The next start goes through `Session::unmasked_daemon`.
    unmasked: bool,
    omarchy: Omarchy,
    session: Session,
}

/// The fixture address ending in `last`.
pub(crate) fn test_address(last: u8) -> BdAddr {
    let [o0, o1, o2, o3, o4] = ADDRESS_PREFIX;
    BdAddr::from([o0, o1, o2, o3, o4, last])
}

impl Rig {
    /// A session, both private buses, the fake BlueZ owning its name and one adapter, the fake logind owning its; no daemon yet.
    pub(crate) fn new() -> Self {
        let mut rig = Self::before_system_bus();
        rig.bring_up_system_bus(true);
        rig
    }

    /// As `new`, but nobody owns `org.bluez` until `bluez().take_name()`.
    pub(crate) fn before_bluez() -> Self {
        let mut rig = Self::before_system_bus();
        rig.bring_up_system_bus(false);
        rig
    }

    /// As `new`, but no system bus runs until `start_system_bus`.
    pub(crate) fn before_system_bus() -> Self {
        let session = Session::new();
        let session_bus = start_bus(&session, SESSION_BUS);
        let omarchy = Omarchy::install(&session, Mood::Works);
        Self {
            daemon: None,
            stopped: false,
            trap: None,
            relay: None,
            audio: None,
            bluez: None,
            adapter: None,
            logind: None,
            system_bus: None,
            session_bus,
            devices: Arc::default(),
            prepare: Vec::new(),
            unmasked: false,
            omarchy,
            session,
        }
    }

    pub(crate) fn start_system_bus(&mut self) {
        self.bring_up_system_bus(true);
    }

    fn bring_up_system_bus(&mut self, own_bluez_name: bool) {
        let bus = start_bus(&self.session, SYSTEM_BUS);
        let mut bluez = FakeBluez::start(&bus.address, own_bluez_name);
        let devices = Arc::clone(&self.devices);
        bluez.on_call(move |bluez, call| react(bluez, call, &devices));
        self.adapter = Some(bluez.add_adapter());
        self.logind = Some(FakeLogind::start(&bus));
        self.bluez = Some(bluez);
        self.system_bus = Some(bus);
    }

    /// The fake audio server in this session; the rig keeps a share so the daemon always ends first.
    pub(crate) fn start_audio(&mut self) -> Rc<FakeAudio> {
        assert!(self.audio.is_none(), "the fake audio server runs already");
        let audio = Rc::new(FakeAudio::start(&self.session.runtime_dir()));
        self.audio = Some(Rc::clone(&audio));
        audio
    }

    /// The fake audio server holding the first pods' sink as its default at `percent`, then those pods linked and the server read.
    pub(crate) fn with_audio(percent: u8) -> (Self, Device, Rc<FakeAudio>) {
        let mut rig = Self::new();
        let audio = rig.start_audio();
        let device = rig.add_pods(Profile::default());
        let sink = sink_named(device.address);
        audio.add_sink(&sink, percent);
        audio.set_default_sink(&sink);
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        rig.wait_audio_server("up");
        (rig, device, audio)
    }

    /// The fake audio server holding every answer, with the first pods' sink, and the daemon started with those pods not yet linked.
    pub(crate) fn with_audio_holding(percent: u8) -> (Self, Device, Rc<FakeAudio>) {
        let mut rig = Self::new();
        let audio = rig.start_audio();
        let device = rig.add_pods(Profile::default());
        audio.add_sink(&sink_named(device.address), percent);
        audio.hold_replies(true);
        rig.start();
        (rig, device, audio)
    }

    pub(crate) fn system_bus_address(&self) -> String {
        bus_address(&self.session, SYSTEM_BUS)
    }

    pub(crate) fn session(&self) -> &Session {
        &self.session
    }

    /// The stand-in `omarchy` on the daemon's path, working until a test makes it `become_` otherwise.
    pub(crate) fn omarchy(&self) -> &Omarchy {
        &self.omarchy
    }

    pub(crate) fn bluez(&self) -> &FakeBluez {
        self.bluez.as_ref().expect("a running system bus")
    }

    pub(crate) fn adapter(&self) -> &AdapterPath {
        self.adapter.as_ref().expect("a running system bus")
    }

    pub(crate) fn logind(&self) -> &FakeLogind {
        self.logind.as_ref().expect("a running system bus")
    }

    /// A second sender on the system bus that never owns the logind name.
    pub(crate) fn impostor(&self) -> Impostor {
        Impostor::connect(self.system_bus.as_ref().expect("a running system bus"))
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

    /// The daemon with one pair of pods, linked and filled.
    pub(crate) fn with_linked_pods() -> (Self, Device) {
        let mut rig = Self::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
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

    /// Hands the daemon, once started, a relay of the system bus that `cut_system_bus` can sever.
    pub(crate) fn relay_system_bus(&mut self) {
        assert!(
            self.daemon.is_none(),
            "the relay must be in place before the daemon starts"
        );
        let path = self.session.dir.path().join(SYSTEM_RELAY_SOCKET);
        let relay = Relay::start(&path, &bus_socket(&self.session, SYSTEM_BUS));
        let address = relay.address();
        self.prepare(move |command| {
            command.env("DBUS_SYSTEM_BUS_ADDRESS", address);
        });
        self.relay = Some(relay);
    }

    /// Closes every system bus connection the daemon holds; the bus, the fakes and the relay's listener live on.
    pub(crate) fn cut_system_bus(&self) {
        self.relay.as_ref().expect("a relayed system bus").cut();
    }

    /// Starts the daemon under umask 0 from the next start on, so the modes it sets are its own.
    pub(crate) fn unmask(&mut self) {
        self.unmasked = true;
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
        let mut command = if self.unmasked {
            self.session.unmasked_daemon()
        } else {
            self.session.command(Path::new(binary()), &["daemon"])
        };
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

    pub(crate) fn player(&self, name: &str, playback: Playback) -> FakePlayer {
        FakePlayer::start(&self.session_bus, name, playback)
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
        self.session.run_client(Path::new(binary()), args)
    }

    /// `librepods status`, parsed.
    pub(crate) fn status(&self) -> Value {
        let output = self.client(&["status"]);
        assert!(output.status.success(), "librepods status: {output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// `librepods watch` in this session, its lines read as they come.
    pub(crate) fn watch(&self) -> Watch {
        Watch::start(&self.session)
    }

    /// Asks `librepods status` until `ready` holds, failing at `HANG_LIMIT`.
    pub(crate) fn wait_status(&self, what: &str, ready: impl Fn(&Value) -> bool) -> Value {
        wait_for(what, || {
            let status = self.status();
            ready(&status).then_some(status)
        })
    }

    /// Asks `librepods status` until `audio_server` reads `word`.
    pub(crate) fn wait_audio_server(&self, word: &str) -> Value {
        self.wait_status(&format!("audio_server {word}"), |status| {
            status["audio_server"] == word
        })
    }

    /// Waits for `up`, for a refresh after a sink came and went (asked only once the sink read at `up` was queued), then for the driver to apply it.
    pub(crate) fn wait_sink_told(&self) {
        self.wait_audio_server("up");
        let audio = self.audio.as_ref().expect("the fake audio server runs");
        let before = audio.requests().len();
        audio.add_sink(PASSING_SINK, PERCENT);
        audio.remove_sink(PASSING_SINK);
        wait_for("a refresh after a sink came and went", || {
            audio.requests()[before..]
                .contains(&AudioAsked::SinkList)
                .then_some(())
        });
        self.wait_queued_inputs_applied();
    }

    /// Returns once the driver applied every input queued before the call, as a command is answered from that queue and a status from the published state; `reopen` only counts `reopen_calls_total`.
    pub(crate) fn wait_queued_inputs_applied(&self) {
        let reply = self.session.ask(b"reopen");
        assert!(
            !reply.is_empty(),
            "reopen, the barrier behind the queued inputs: no reply, so the driver did not answer it"
        );
    }

    /// Asks `librepods status` for `duration`, failing at the first answer where `check` does not hold; the first is checked whenever it comes, as every caller's state outlasts the window.
    pub(crate) fn holds_for(&self, duration: Duration, what: &str, check: impl Fn(&Value) -> bool) {
        let started = Instant::now();
        loop {
            let status = self.status();
            assert!(
                check(&status),
                "{what} ended after {:?}: {status}",
                started.elapsed()
            );
            if started.elapsed() >= duration {
                return;
            }
        }
    }

    /// Waits for the link up and the pods' name in the status, which the burst brings after the batteries and the ears.
    pub(crate) fn wait_filled(&self) -> Value {
        self.wait_status("the link up and the pods' name", |status| {
            connected(status) && named(status)
        })
    }

    /// Takes `do disconnect`, then waits for the name to clear, which only the reset does.
    pub(crate) fn disconnect(&self) {
        let output = self.client(&["do", "disconnect"]);
        assert!(output.status.success(), "do disconnect: {output:?}");
        self.wait_status("the name cleared", |status| status["device_name"] == "");
    }

    /// Closes the control channel with every redial refused while BlueZ keeps `device` connected.
    pub(crate) fn hang_up_channel(&self, device: &Device) {
        let refused = Answer::Refuse {
            after: Duration::ZERO,
            errno: Errno::CONNREFUSED,
        };
        self.answer(device, refused);
        device.pods.hang_up();
    }

    /// `hang_up_channel`, then waits for the status to show it.
    pub(crate) fn close_channel(&self, device: &Device) {
        self.hang_up_channel(device);
        // A loss would clear the name, so a kept name shows the channel closed and the link was not lost.
        self.wait_status("the channel closed with the name kept", |status| {
            !connected(status) && status["device_name"] != ""
        });
    }

    /// Links `device` again after a loss and waits for it to fill the state.
    pub(crate) fn relink(&self, device: &Device) {
        self.link_up(device);
        self.wait_filled();
    }

    pub(crate) fn daemon(&mut self) -> &mut Daemon {
        self.daemon.as_mut().expect("a started daemon")
    }

    /// Waits for a line of the daemon's log equal to `line`.
    pub(crate) fn wait_logged(&mut self, line: &str) {
        let daemon = self.daemon();
        wait_for(&format!("the daemon to log `{line}`"), || {
            daemon
                .log()
                .lines()
                .any(|logged| logged == line)
                .then_some(())
        });
    }

    /// Fails unless the daemon has yet to log `line`; the daemon runs on one thread, so this orders `what` before the logged work's end.
    pub(crate) fn assert_not_logged(&mut self, what: &str, line: &str) {
        let log = self.daemon().log();
        assert!(
            !log.lines().any(|logged| logged == line),
            "{what}: the daemon logged `{line}` already"
        );
    }

    /// Waits for the link to have entered `phase` `times` times, from whichever phase the racing words of BlueZ and of the channel took it through.
    pub(crate) fn wait_entered(&mut self, phase: &str, times: usize) {
        wait_for(
            &format!("the link to enter `{phase}` {times} times"),
            || (self.entered(phase) >= times).then_some(()),
        );
    }

    /// How often the daemon's log shows the link entering `phase`; sample input `link: up to grace`.
    pub(crate) fn entered(&mut self, phase: &str) -> usize {
        let entered = format!(" to {phase}");
        let log = self.daemon().log();
        let steps = log.lines().filter(|line| line.starts_with("link: "));
        steps.filter(|line| line.ends_with(&entered)).count()
    }

    fn trap(&self) -> &Trap {
        self.trap.as_ref().expect("a daemon started under the trap")
    }

    /// Sends SIGTERM as a test's own step and gives the exit status, which must come within `limit`.
    pub(crate) fn stop(&mut self, limit: Duration) -> ExitStatus {
        self.term();
        self.exited(limit)
    }

    /// Sends SIGTERM as a test's own step, leaving the exit to `exited`.
    pub(crate) fn term(&mut self) {
        kill_process(self.daemon().pid(), Signal::TERM).unwrap();
    }

    /// The exit status, which must come within `limit`.
    pub(crate) fn exited(&mut self, limit: Duration) -> ExitStatus {
        let daemon = self.daemon();
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
                let status = daemon.stop();
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
    let device = |path: &str| {
        let found = lock(devices)
            .iter()
            .find(|device| device.path.0 == path)
            .cloned();
        found.expect("a call on a device of the rig")
    };
    match call.method {
        DeviceMethod::Connect => link_up(bluez, &device(&call.object).path),
        DeviceMethod::Disconnect => {
            let device = device(&call.object);
            link_down(bluez, &device.path, &device.pods);
        }
        DeviceMethod::RemoveDevice => {
            let device = device(call.argument.as_deref().expect("the device path"));
            bluez.remove(&device.path);
            device.pods.hang_up();
        }
        DeviceMethod::StartDiscovery
        | DeviceMethod::StopDiscovery
        | DeviceMethod::SetDiscoveryFilter
        | DeviceMethod::RegisterMonitor
        | DeviceMethod::UnregisterMonitor
        | DeviceMethod::Set { .. } => {}
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

/// The media adapter's line once a pause's work has ended and its report is queued.
pub(crate) fn pause_ended(players: usize) -> String {
    format!("media: the pause ended with {players} players paused")
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
pub(crate) fn assert_gap(
    what: &str,
    from: Instant,
    to: Instant,
    range: &(impl RangeBounds<Duration> + Debug),
) {
    let gap = to.checked_duration_since(from);
    eprintln!("{what}: {gap:?}, wanted {range:?}");
    assert!(
        gap.is_some_and(|gap| range.contains(&gap)),
        "{what}: {gap:?}, wanted {range:?}"
    );
}
