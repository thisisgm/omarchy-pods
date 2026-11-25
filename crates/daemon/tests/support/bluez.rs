//! A fake BlueZ on a private bus: the object manager at `/`, adapters and devices, served from one thread of its own.

mod barrier;
mod calls;
pub(crate) mod monitor;
pub(crate) mod transport;

use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::ops::Deref;
use std::panic;
use std::pin::Pin;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pods_proto::addr::BdAddr;
use tokio::runtime::{Builder as RuntimeBuilder, Handle};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::timeout;
use zbus::connection::Builder;
use zbus::export::futures_core::Stream;
use zbus::export::serde::Serialize;
use zbus::message::Type;
use zbus::names::{InterfaceName, MemberName};
use zbus::zvariant::DynamicType;
use zbus::{
    Connection, Message, MessageStream, ObjectPath, OwnedObjectPath, OwnedVariant, Str, Variant,
};

use super::HANG_LIMIT;
use barrier::Barrier;
use calls::record;
pub(crate) use calls::{Call, DeviceMethod};
use monitor::{MANAGER_INTERFACE, Monitors, Registration};
pub(crate) use transport::TransportPath;
use transport::{TRANSPORT_INTERFACE, Transports};

/// The service UUID AirPods list, the one the daemon looks for.
pub(crate) const AAP_UUID: &str = "74ec2172-0bad-4d01-8f77-997b2be0722a";

pub(crate) const DEVICE_INTERFACE: &str = "org.bluez.Device1";
const ADAPTER_INTERFACE: &str = "org.bluez.Adapter1";
const BLUEZ_NAME: &str = "org.bluez";
const ADAPTER_PREFIX: &str = "/org/bluez/";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const FAILED: &str = "org.bluez.Error.Failed";
const UNKNOWN: &str = "org.freedesktop.DBus.Error.UnknownMethod";
const BUS_DRIVER: &str = "org.freedesktop.DBus";
/// Every name leaving the bus, which ends the registrations its owner made.
const NAMES_LEAVING: &str = "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged'";
const THREAD_NAME: &str = "fake-bluez";
const REACTOR_THREAD: &str = "fake-bluez-reactions";
/// Answered calls waiting for their reaction; a test makes a handful.
const REACTIONS_QUEUE: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdapterPath(pub(crate) String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DevicePath(pub(crate) String);

/// The serving thread and the sender that ends it.
type Server = (oneshot::Sender<()>, JoinHandle<()>);

/// The fake's connection, its runtime and its objects, which the reactions to calls share.
#[derive(Clone)]
pub(crate) struct Bluez {
    connection: Connection,
    handle: Handle,
    objects: Arc<Mutex<Objects>>,
}

/// Declared in the order they must end: the reactions use the server.
pub(crate) struct FakeBluez {
    bluez: Bluez,
    reactor: Option<JoinHandle<()>>,
    server: Option<Server>,
}

#[derive(Default)]
struct Objects {
    adapters: Vec<Adapter>,
    devices: HashMap<String, Device>,
    transports: Transports,
    monitors: Monitors,
    /// Every change of a device object's connection, its adding and its removal included, oldest first.
    edges: Vec<Edge>,
    /// Each call recorded with when it reached the fake.
    calls: Vec<(Instant, Call)>,
    hold: bool,
    fail: bool,
    /// When each `GetManagedObjects` call reached the fake, held ones included.
    sweeps: Vec<Instant>,
    barrier: Barrier,
    reactions: Option<SyncSender<Call>>,
}

struct Adapter {
    path: String,
    powered: bool,
}

/// What the serving thread does with a call: send these messages in order, the reply last, send them late, send nothing, or read an application.
enum Answer {
    Send(Vec<Message>),
    Late(Vec<Message>),
    Held,
    Register(Registration),
}

struct Device {
    adapter: String,
    address: BdAddr,
    uuids: Vec<String>,
    connected: bool,
    paired: bool,
    modalias: Option<String>,
}

struct Edge {
    at: Instant,
    address: BdAddr,
    path: String,
    connected: bool,
}

/// The body of `GetManagedObjects`: path, then interface, then property.
type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedVariant>>>;

impl FakeBluez {
    /// Serves the object manager at `/` on the bus at `bus_address`, owning `org.bluez` when `own_name` is set.
    pub(crate) fn start(bus_address: &str, own_name: bool) -> Self {
        let objects = Arc::new(Mutex::new(Objects::default()));
        let (stop, stopped) = oneshot::channel();
        let (ready, started) = mpsc::channel();
        let address = bus_address.to_owned();
        let shared = Arc::clone(&objects);
        let thread = thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || run(&address, own_name, &shared, stopped, &ready))
            .unwrap();
        let (connection, handle) = started
            .recv_timeout(HANG_LIMIT)
            .expect("the fake BlueZ on the bus");
        Self {
            bluez: Bluez {
                connection,
                handle,
                objects,
            },
            reactor: None,
            server: Some((stop, thread)),
        }
    }

    /// Runs `reaction` on a thread of its own after each device or adapter call the fake answers with success, in order.
    pub(crate) fn on_call(&mut self, reaction: impl Fn(&Bluez, &Call) + Send + 'static) {
        let (sender, answered) = mpsc::sync_channel(REACTIONS_QUEUE);
        self.lock().reactions = Some(sender);
        let bluez = self.bluez.clone();
        let thread = thread::Builder::new()
            .name(REACTOR_THREAD.to_owned())
            .spawn(move || {
                for call in answered {
                    reaction(&bluez, &call);
                }
            })
            .unwrap();
        self.reactor = Some(thread);
    }

    /// Whether the serving thread has ended on its own.
    pub(crate) fn stopped_serving(&self) -> bool {
        self.server
            .as_ref()
            .is_none_or(|(_, thread)| thread.is_finished())
    }

    /// Ends the reactor once the reactions already queued ran; they emit through the server, so it ends first.
    fn end_reactions(&mut self) -> Option<thread::Result<()>> {
        self.lock().reactions = None;
        self.reactor.take().map(JoinHandle::join)
    }

    /// Ends the reactions and the serving thread, failing the test if either panicked or the serving stopped before.
    pub(crate) fn finish(mut self) {
        if let Some(Err(panic)) = self.end_reactions() {
            panic::resume_unwind(panic);
        }
        let (stop, thread) = self.server.take().expect("a serving fake BlueZ");
        assert!(
            !thread.is_finished(),
            "the fake BlueZ stopped serving before the end of the test"
        );
        // A thread gone by now ended in a panic, which the join reports.
        stop.send(()).ok();
        assert!(thread.join().is_ok(), "the fake BlueZ panicked");
    }
}

impl Deref for FakeBluez {
    type Target = Bluez;

    fn deref(&self) -> &Bluez {
        &self.bluez
    }
}

impl Bluez {
    /// Adds a powered adapter named after the count of adapters before it: `hci0` first.
    pub(crate) fn add_adapter(&self) -> AdapterPath {
        let count = self.lock().adapters.len();
        self.add_adapter_at(&format!("hci{count}"))
    }

    /// Adds a powered adapter at `/org/bluez/<name>`.
    pub(crate) fn add_adapter_at(&self, name: &str) -> AdapterPath {
        let adapter = Adapter {
            path: format!("{ADAPTER_PREFIX}{name}"),
            powered: true,
        };
        let interfaces = HashMap::from([(ADAPTER_INTERFACE.to_owned(), adapter.properties())]);
        let path = adapter.path.clone();
        {
            let mut objects = self.lock();
            let taken = objects.adapters.iter().any(|known| known.path == path);
            assert!(!taken, "the fake already has an adapter at {path}");
            objects.adapters.push(adapter);
        }
        self.emit(
            "/",
            OBJECT_MANAGER,
            "InterfacesAdded",
            &(object(&path), interfaces),
        );
        AdapterPath(path)
    }

    /// Powers the adapter on or off and emits `PropertiesChanged` for it; an adapter off hears no advert.
    pub(crate) fn power(&self, adapter: &AdapterPath, on: bool) {
        let lost = {
            let mut objects = self.lock();
            let found = objects
                .adapters
                .iter_mut()
                .find(|known| known.path == adapter.0);
            found.expect("an adapter of the fake").powered = on;
            if on {
                Vec::new()
            } else {
                objects.monitors.power_down(&adapter.0)
            }
        };
        let changed = HashMap::from([("Powered", OwnedVariant::from(on))]);
        let body = (ADAPTER_INTERFACE, changed, Vec::<String>::new());
        self.emit(&adapter.0, PROPERTIES, "PropertiesChanged", &body);
        self.send(lost);
    }

    pub(crate) fn add_device(
        &self,
        adapter: &AdapterPath,
        address: BdAddr,
        uuids: &[&str],
        connected: bool,
    ) -> DevicePath {
        let path = format!("{}/dev_{}", adapter.0, octets(address, "_"));
        let device = Device {
            adapter: adapter.0.clone(),
            address,
            uuids: uuids.iter().map(|uuid| (*uuid).to_owned()).collect(),
            connected,
            paired: true,
            modalias: None,
        };
        let interfaces = HashMap::from([(DEVICE_INTERFACE.to_owned(), device.properties())]);
        {
            let mut objects = self.lock();
            objects.edge(&path, address, connected);
            objects.devices.insert(path.clone(), device);
        }
        self.emit(
            "/",
            OBJECT_MANAGER,
            "InterfacesAdded",
            &(object(&path), interfaces),
        );
        DevicePath(path)
    }

    /// Changes one property of a device and emits `PropertiesChanged` for it.
    pub(crate) fn set(&self, path: &DevicePath, property: &str, value: OwnedVariant) {
        let dropped = {
            let mut objects = self.lock();
            let Objects {
                devices,
                edges,
                transports,
                ..
            } = &mut *objects;
            let device = devices.get_mut(&path.0).expect("a device of the fake");
            let mut dropped = Vec::new();
            match property {
                "Connected" => {
                    device.connected = bool::try_from(&value).unwrap();
                    edges.push(Edge::now(&path.0, device.address, device.connected));
                    if !device.connected {
                        dropped = transports.remove_device(&path.0);
                    }
                }
                "UUIDs" => device.uuids = Vec::try_from(value.try_clone().unwrap()).unwrap(),
                "Paired" => device.paired = bool::try_from(&value).unwrap(),
                "Modalias" => {
                    device.modalias = Some(String::try_from(value.try_clone().unwrap()).unwrap());
                }
                _ => {}
            }
            dropped
        };
        // corner: a disconnect closes the device's streams, so its transports leave first
        self.transports_removed(dropped);
        let changed = HashMap::from([(property, value)]);
        let body = (DEVICE_INTERFACE, changed, Vec::<String>::new());
        self.emit(&path.0, PROPERTIES, "PropertiesChanged", &body);
    }

    /// Emits a `PropertiesChanged` for a device whose body is not the one the interface defines.
    pub(crate) fn emit_unreadable(&self, path: &DevicePath) {
        let body = (DEVICE_INTERFACE, 7_u32);
        self.emit(&path.0, PROPERTIES, "PropertiesChanged", &body);
    }

    /// Adds a `MediaTransport1` under the device and emits `InterfacesAdded` for it.
    pub(crate) fn add_transport(
        &self,
        device: &DevicePath,
        codec: u8,
        configuration: &[u8],
    ) -> TransportPath {
        let (path, interfaces) = {
            let mut objects = self.lock();
            assert!(
                objects.devices.contains_key(&device.0),
                "a device of the fake"
            );
            let path = objects.transports.add(&device.0, codec, configuration);
            let interfaces = objects.transports.interfaces(&path).unwrap();
            (path, interfaces)
        };
        self.emit(
            "/",
            OBJECT_MANAGER,
            "InterfacesAdded",
            &(object(&path), interfaces),
        );
        TransportPath(path)
    }

    /// Removes a device and emits `InterfacesRemoved` for it, after one for each of its transports as BlueZ does.
    pub(crate) fn remove(&self, path: &DevicePath) {
        let transports = {
            let mut objects = self.lock();
            let device = objects
                .devices
                .remove(&path.0)
                .expect("a device of the fake");
            objects.edge(&path.0, device.address, false);
            objects.transports.remove_device(&path.0)
        };
        self.transports_removed(transports);
        let body = (object(&path.0), vec![DEVICE_INTERFACE]);
        self.emit("/", OBJECT_MANAGER, "InterfacesRemoved", &body);
    }

    fn transports_removed(&self, paths: Vec<String>) {
        for path in paths {
            let body = (object(&path), vec![TRANSPORT_INTERFACE]);
            self.emit("/", OBJECT_MANAGER, "InterfacesRemoved", &body);
        }
    }

    pub(crate) fn drop_name(&self) {
        let released = self
            .handle
            .block_on(self.connection.release_name(BLUEZ_NAME));
        assert!(released.unwrap(), "the fake BlueZ did not own {BLUEZ_NAME}");
    }

    pub(crate) fn take_name(&self) {
        self.handle
            .block_on(self.connection.request_name(BLUEZ_NAME))
            .unwrap();
    }

    /// While set, no method call is answered.
    pub(crate) fn hold_replies(&self, hold: bool) {
        self.lock().hold = hold;
    }

    /// While set, every method call is answered with `org.bluez.Error.Failed`.
    pub(crate) fn fail_calls(&self, fail: bool) {
        self.lock().fail = fail;
    }

    /// When each sweep reached the fake, oldest first.
    pub(crate) fn sweeps(&self) -> Vec<Instant> {
        self.lock().sweeps.clone()
    }

    /// Every `Device1` and `Adapter1` method call, oldest first.
    pub(crate) fn calls(&self) -> Vec<Call> {
        self.lock()
            .calls
            .iter()
            .map(|(_, call)| call.clone())
            .collect()
    }

    /// When each call of `method` reached the fake, oldest first.
    pub(crate) fn call_times(&self, method: &DeviceMethod) -> Vec<Instant> {
        let objects = self.lock();
        let of_method = objects
            .calls
            .iter()
            .filter(|(_, call)| call.method == *method);
        of_method.map(|(at, _)| *at).collect()
    }

    /// Calls `method` on the device's `Device1` over the bus, as the daemon would.
    pub(crate) fn call_device(&self, path: &DevicePath, method: &str) {
        let call = self.connection.call_method(
            Some(BLUEZ_NAME),
            path.0.as_str(),
            Some(DEVICE_INTERFACE),
            method,
            &(),
        );
        self.handle
            .block_on(async { timeout(HANG_LIMIT, call).await })
            .unwrap_or_else(|_| panic!("{method} unanswered after {HANG_LIMIT:?}"))
            .unwrap();
    }

    /// Whether any object of `address` was connected at `at` or within `slack` before it.
    pub(crate) fn connected_at(&self, address: BdAddr, at: Instant, slack: Duration) -> bool {
        let from = at.checked_sub(slack).unwrap_or(at);
        let objects = self.lock();
        let mut paths: Vec<&str> = objects
            .edges
            .iter()
            .filter(|edge| edge.address == address)
            .map(|edge| edge.path.as_str())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        paths.into_iter().any(|path| {
            let mut connected_at_from = false;
            for edge in objects.edges.iter().filter(|edge| edge.path == path) {
                if edge.at <= from {
                    connected_at_from = edge.connected;
                } else if edge.at <= at && edge.connected {
                    return true;
                }
            }
            connected_at_from
        })
    }

    fn lock(&self) -> MutexGuard<'_, Objects> {
        self.objects.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(
        &self,
        path: &str,
        interface: &str,
        member: &str,
        body: &(impl Serialize + DynamicType),
    ) {
        let emitted = self
            .connection
            .emit_signal(None::<&str>, path, interface, member, body);
        self.handle.block_on(emitted).unwrap();
    }

    fn send(&self, messages: Vec<Message>) {
        self.handle.block_on(async {
            for message in messages {
                self.connection.send(&message).await.unwrap();
            }
        });
    }
}

impl Drop for FakeBluez {
    fn drop(&mut self) {
        let reacted = self.end_reactions();
        let served = self.server.take().map(|(stop, thread)| {
            // A thread that already ended dropped its receiver, which is the end wanted.
            stop.send(()).ok();
            thread.join()
        });
        for ended in [reacted, served].into_iter().flatten() {
            if let Err(panic) = ended
                && !thread::panicking()
            {
                panic::resume_unwind(panic);
            }
        }
    }
}

impl Objects {
    fn edge(&mut self, path: &str, address: BdAddr, connected: bool) {
        self.edges.push(Edge::now(path, address, connected));
    }

    fn managed(&self) -> Managed {
        let adapters = self.adapters.iter().map(|adapter| {
            let mut interfaces =
                HashMap::from([(ADAPTER_INTERFACE.to_owned(), adapter.properties())]);
            interfaces.extend(self.monitors.manager_of(&adapter.path));
            (object(&adapter.path).into(), interfaces)
        });
        let devices = self.devices.iter().map(|(path, device)| {
            let interfaces = HashMap::from([(DEVICE_INTERFACE.to_owned(), device.properties())]);
            (object(path).into(), interfaces)
        });
        adapters
            .chain(devices)
            .chain(self.transports.managed())
            .chain(self.monitors.managed_devices())
            .collect()
    }

    /// The properties `Get` and `GetAll` read at `path`, a device's, a transport's or an adapter's.
    fn properties(&self, path: &str) -> Option<HashMap<String, OwnedVariant>> {
        let adapter = || {
            let adapter = self.adapters.iter().find(|adapter| adapter.path == path)?;
            Some(adapter.properties())
        };
        self.devices
            .get(path)
            .map(Device::properties)
            .or_else(|| self.transports.properties(path))
            .or_else(|| self.monitors.device_properties(path))
            .or_else(adapter)
            .or_else(|| Barrier::properties(path))
    }

    /// The answer to `call`; a sweep's arrival and a device or adapter method are recorded first.
    fn answer(&mut self, call: &Message) -> Answer {
        let header = call.header();
        if header.member().map(MemberName::as_str) == Some("GetManagedObjects") {
            self.sweeps.push(Instant::now());
        }
        self.barrier.note(&header);
        let recorded = record(call);
        if let Some(recorded) = &recorded {
            self.calls.push((Instant::now(), recorded.clone()));
        }
        if self.hold {
            return Answer::Held;
        }
        if self.fail {
            let failed = Message::error(&header, FAILED).build(&("Failed",)).unwrap();
            return Answer::Send(vec![failed]);
        }
        let interface = header.interface().map(InterfaceName::as_str);
        let member = header.member().map(MemberName::as_str);
        let path = header.path().map(ObjectPath::as_str);
        let reply = match (interface, member, path) {
            (Some(MANAGER_INTERFACE), Some(member), Some(path)) if self.monitors.serves(path) => {
                return self.monitors.answer(call, path, member);
            }
            (Some(MANAGER_INTERFACE), _, _) => {
                Message::error(&header, UNKNOWN).build(&("No manager on this adapter",))
            }
            (Some(OBJECT_MANAGER), Some("GetManagedObjects"), Some("/")) => {
                Message::method_return(&header).build(&self.managed())
            }
            _ if recorded.is_some() => {
                if let (Some(recorded), Some(reactions)) = (recorded, &self.reactions)
                    && let Err(error) = reactions.try_send(recorded)
                {
                    panic!("the reaction to a call was lost: {error}");
                }
                Message::method_return(&header).build(&())
            }
            (Some(PROPERTIES), Some("GetAll"), Some(path)) => match self.properties(path) {
                Some(properties) => {
                    let reply = Message::method_return(&header).build(&properties).unwrap();
                    // Sent on this connection ahead of the reply, so the daemon holds the change before the read can end.
                    let mut messages = Vec::from_iter(self.monitors.advert_at_read(path));
                    messages.push(reply);
                    return Answer::Send(messages);
                }
                None => Message::error(&header, UNKNOWN).build(&("No such object",)),
            },
            (Some(PROPERTIES), Some("Get"), Some(path)) => {
                let property = call
                    .body()
                    .deserialize::<(String, String)>()
                    .ok()
                    .and_then(|(_, name)| self.properties(path)?.remove(&name));
                match property {
                    Some(value) => Message::method_return(&header).build(&value),
                    None => Message::error(&header, UNKNOWN).build(&("No such property",)),
                }
            }
            _ => Message::error(&header, UNKNOWN).build(&("Not served by the fake",)),
        };
        Answer::Send(vec![reply.unwrap()])
    }
}

impl Adapter {
    fn properties(&self) -> HashMap<String, OwnedVariant> {
        HashMap::from([("Powered".to_owned(), OwnedVariant::from(self.powered))])
    }
}

impl Device {
    /// Every property of `Device1` the daemon may read, and a name holding the address that it must never log.
    fn properties(&self) -> HashMap<String, OwnedVariant> {
        let address = octets(self.address, ":");
        let uuids = OwnedVariant::try_from(Variant::from(self.uuids.clone())).unwrap();
        let modalias = self.modalias.iter().map(|modalias| {
            (
                "Modalias".to_owned(),
                OwnedVariant::from(Str::from(modalias.clone())),
            )
        });
        let mut properties = HashMap::from([
            (
                "Address".to_owned(),
                OwnedVariant::from(Str::from(address.clone())),
            ),
            (
                "Name".to_owned(),
                OwnedVariant::from(Str::from(format!("Pods {address}"))),
            ),
            (
                "Adapter".to_owned(),
                OwnedVariant::from(object(&self.adapter)),
            ),
            ("Paired".to_owned(), OwnedVariant::from(self.paired)),
            ("UUIDs".to_owned(), uuids),
            ("Connected".to_owned(), OwnedVariant::from(self.connected)),
            (
                "ServicesResolved".to_owned(),
                OwnedVariant::from(self.connected),
            ),
        ]);
        properties.extend(modalias);
        properties
    }
}

impl Edge {
    fn now(path: &str, address: BdAddr, connected: bool) -> Self {
        Self {
            at: Instant::now(),
            address,
            path: path.to_owned(),
            connected,
        }
    }
}

fn object(path: &str) -> ObjectPath<'static> {
    ObjectPath::try_from(path.to_owned()).unwrap()
}

/// The address as BlueZ writes it, upper case, with `separator` between octets.
fn octets(address: BdAddr, separator: &str) -> String {
    address
        .octets()
        .map(|octet| format!("{octet:02X}"))
        .join(separator)
}

/// The unique name a `NameOwnerChanged` of the bus says has left; sample body `(":1.7", ":1.7", "")`.
fn name_left(message: &Message) -> Option<String> {
    let header = message.header();
    let signal = message.message_type() == Type::Signal
        && header.interface().map(InterfaceName::as_str) == Some(BUS_DRIVER)
        && header.member().map(MemberName::as_str) == Some("NameOwnerChanged");
    if !signal {
        return None;
    }
    let (name, _, owner): (String, String, String) = message.body().deserialize().ok()?;
    owner.is_empty().then_some(name)
}

/// What the serving thread waits on: the stop, the next message, or a registration read that ended.
enum Event {
    Stop,
    Message(Option<zbus::Result<Message>>),
    Read(Result<(), tokio::task::JoinError>),
}

/// The thread's body: connects, owns the name if asked, hands the connection back, then answers calls until stopped.
fn run(
    address: &str,
    own_name: bool,
    objects: &Arc<Mutex<Objects>>,
    mut stopped: oneshot::Receiver<()>,
    ready: &mpsc::Sender<(Connection, Handle)>,
) {
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let connection = Builder::address(address).build().await.unwrap();
        let mut calls = MessageStream::from(&connection);
        let driver = Some(BUS_DRIVER);
        let watch = connection.call_method(
            driver,
            "/org/freedesktop/DBus",
            driver,
            "AddMatch",
            &(NAMES_LEAVING,),
        );
        watch.await.unwrap();
        if own_name {
            connection.request_name(BLUEZ_NAME).await.unwrap();
        }
        ready.send((connection.clone(), Handle::current())).unwrap();
        let mut registrations = JoinSet::new();
        loop {
            let next = poll_fn(|context| {
                if Pin::new(&mut stopped).poll(context).is_ready() {
                    return Poll::Ready(Event::Stop);
                }
                if let Poll::Ready(Some(read)) = registrations.poll_join_next(context) {
                    return Poll::Ready(Event::Read(read));
                }
                Pin::new(&mut calls).poll_next(context).map(Event::Message)
            });
            let call = match next.await {
                Event::Stop => {
                    while let Some(read) = registrations.try_join_next() {
                        if let Err(error) = read
                            && error.is_panic()
                        {
                            panic::resume_unwind(error.into_panic());
                        }
                    }
                    return;
                }
                Event::Read(Ok(())) => continue,
                Event::Read(Err(error)) => panic::resume_unwind(error.into_panic()),
                Event::Message(Some(Ok(call))) => call,
                Event::Message(Some(Err(error))) => {
                    panic!("the fake BlueZ cannot read its bus: {error}")
                }
                Event::Message(None) => {
                    panic!("the fake BlueZ's bus closed before the end of the test")
                }
            };
            let answer = {
                let mut state = objects.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(owner) = name_left(&call) {
                    state.monitors.forget(&owner);
                    continue;
                }
                if call.message_type() != Type::MethodCall {
                    continue;
                }
                state.answer(&call)
            };
            match answer {
                Answer::Send(messages) => {
                    for message in messages {
                        connection.send(&message).await.unwrap();
                    }
                }
                Answer::Late(messages) => {
                    registrations.spawn(monitor::answer_late(connection.clone(), messages));
                }
                Answer::Held => {}
                Answer::Register(registration) => {
                    let shared = Arc::clone(objects);
                    registrations.spawn(monitor::register(
                        connection.clone(),
                        shared,
                        registration,
                    ));
                }
            }
        }
    });
}
