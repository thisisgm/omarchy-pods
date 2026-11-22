//! A fake BlueZ on a private bus: the object manager at `/`, adapters and devices, served from one thread of its own.

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

/// The service UUID AirPods list, the one the daemon looks for.
pub(crate) const AAP_UUID: &str = "74ec2172-0bad-4d01-8f77-997b2be0722a";

pub(crate) const DEVICE_INTERFACE: &str = "org.bluez.Device1";
const ADAPTER_INTERFACE: &str = "org.bluez.Adapter1";
const BLUEZ_NAME: &str = "org.bluez";
const ADAPTER_PREFIX: &str = "/org/bluez/hci";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const FAILED: &str = "org.bluez.Error.Failed";
const UNKNOWN: &str = "org.freedesktop.DBus.Error.UnknownMethod";
const THREAD_NAME: &str = "fake-bluez";
const REACTOR_THREAD: &str = "fake-bluez-reactions";
/// Answered calls waiting for their reaction; a test makes a handful.
const REACTIONS_QUEUE: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdapterPath(pub(crate) String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DevicePath(pub(crate) String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceMethod {
    Connect,
    Disconnect,
    RemoveDevice,
}

/// A method call on a device or an adapter; `argument` is the device path a `RemoveDevice` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Call {
    pub(crate) object: String,
    pub(crate) method: DeviceMethod,
    pub(crate) argument: Option<String>,
}

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
    adapters: Vec<String>,
    devices: HashMap<String, Device>,
    /// Every change of a device object's connection, its adding and its removal included, oldest first.
    edges: Vec<Edge>,
    calls: Vec<Call>,
    hold: bool,
    fail: bool,
    /// When each `GetManagedObjects` call reached the fake, held ones included.
    sweeps: Vec<Instant>,
    reactions: Option<SyncSender<Call>>,
}

struct Device {
    adapter: String,
    address: BdAddr,
    uuids: Vec<String>,
    connected: bool,
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
    pub(crate) fn add_adapter(&self) -> AdapterPath {
        let path = {
            let mut objects = self.lock();
            let path = format!("{ADAPTER_PREFIX}{}", objects.adapters.len());
            objects.adapters.push(path.clone());
            path
        };
        let interfaces = HashMap::from([(ADAPTER_INTERFACE, HashMap::<&str, OwnedVariant>::new())]);
        self.emit(
            "/",
            OBJECT_MANAGER,
            "InterfacesAdded",
            &(object(&path), interfaces),
        );
        AdapterPath(path)
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
        {
            let mut objects = self.lock();
            let Objects { devices, edges, .. } = &mut *objects;
            let device = devices.get_mut(&path.0).expect("a device of the fake");
            match property {
                "Connected" => {
                    device.connected = bool::try_from(&value).unwrap();
                    edges.push(Edge::now(&path.0, device.address, device.connected));
                }
                "UUIDs" => device.uuids = Vec::try_from(value.try_clone().unwrap()).unwrap(),
                _ => {}
            }
        }
        let changed = HashMap::from([(property, value)]);
        let body = (DEVICE_INTERFACE, changed, Vec::<String>::new());
        self.emit(&path.0, PROPERTIES, "PropertiesChanged", &body);
    }

    /// Emits a `PropertiesChanged` for a device whose body is not the one the interface defines.
    pub(crate) fn emit_unreadable(&self, path: &DevicePath) {
        let body = (DEVICE_INTERFACE, 7_u32);
        self.emit(&path.0, PROPERTIES, "PropertiesChanged", &body);
    }

    /// Removes a device and emits `InterfacesRemoved` for it.
    pub(crate) fn remove(&self, path: &DevicePath) {
        {
            let mut objects = self.lock();
            let device = objects
                .devices
                .remove(&path.0)
                .expect("a device of the fake");
            objects.edge(&path.0, device.address, false);
        }
        let body = (object(&path.0), vec![DEVICE_INTERFACE]);
        self.emit("/", OBJECT_MANAGER, "InterfacesRemoved", &body);
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
        self.lock().calls.clone()
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
        let adapters = self.adapters.iter().map(|path| {
            let interfaces = HashMap::from([(ADAPTER_INTERFACE.to_owned(), HashMap::new())]);
            (object(path).into(), interfaces)
        });
        let devices = self.devices.iter().map(|(path, device)| {
            let interfaces = HashMap::from([(DEVICE_INTERFACE.to_owned(), device.properties())]);
            (object(path).into(), interfaces)
        });
        adapters.chain(devices).collect()
    }

    /// The reply to `call`, or none while replies are held; a sweep's arrival and a device or adapter method are recorded first.
    fn answer(&mut self, call: &Message) -> Option<Message> {
        let header = call.header();
        if header.member().map(MemberName::as_str) == Some("GetManagedObjects") {
            self.sweeps.push(Instant::now());
        }
        let recorded = record(call);
        if let Some(recorded) = &recorded {
            self.calls.push(recorded.clone());
        }
        if self.hold {
            return None;
        }
        if self.fail {
            return Some(Message::error(&header, FAILED).build(&("Failed",)).unwrap());
        }
        let interface = header.interface().map(InterfaceName::as_str);
        let member = header.member().map(MemberName::as_str);
        let path = header.path().map(ObjectPath::as_str);
        let reply = match (interface, member, path) {
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
            (Some(PROPERTIES), Some("GetAll"), Some(path)) => match self.devices.get(path) {
                Some(device) => Message::method_return(&header).build(&device.properties()),
                None => Message::error(&header, UNKNOWN).build(&("No such object",)),
            },
            (Some(PROPERTIES), Some("Get"), Some(path)) => {
                let property = call
                    .body()
                    .deserialize::<(String, String)>()
                    .ok()
                    .and_then(|(_, name)| self.devices.get(path)?.properties().remove(&name));
                match property {
                    Some(value) => Message::method_return(&header).build(&value),
                    None => Message::error(&header, UNKNOWN).build(&("No such property",)),
                }
            }
            _ => Message::error(&header, UNKNOWN).build(&("Not served by the fake",)),
        };
        Some(reply.unwrap())
    }
}

impl Device {
    /// Every property of `Device1` the daemon may read, and a name holding the address that it must never log.
    fn properties(&self) -> HashMap<String, OwnedVariant> {
        let address = octets(self.address, ":");
        let uuids = OwnedVariant::try_from(Variant::from(self.uuids.clone())).unwrap();
        HashMap::from([
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
            ("Paired".to_owned(), OwnedVariant::from(true)),
            ("UUIDs".to_owned(), uuids),
            ("Connected".to_owned(), OwnedVariant::from(self.connected)),
            (
                "ServicesResolved".to_owned(),
                OwnedVariant::from(self.connected),
            ),
        ])
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

/// The call as `calls` lists it, when it is `Connect` or `Disconnect` on `Device1` or `RemoveDevice` on `Adapter1`.
fn record(call: &Message) -> Option<Call> {
    let header = call.header();
    let interface = header.interface().map(InterfaceName::as_str);
    let member = header.member().map(MemberName::as_str);
    let (method, argument) = match (interface, member) {
        (Some(DEVICE_INTERFACE), Some("Connect")) => (DeviceMethod::Connect, None),
        (Some(DEVICE_INTERFACE), Some("Disconnect")) => (DeviceMethod::Disconnect, None),
        (Some(ADAPTER_INTERFACE), Some("RemoveDevice")) => {
            let device: OwnedObjectPath = call.body().deserialize().ok()?;
            (DeviceMethod::RemoveDevice, Some(device.as_str().to_owned()))
        }
        _ => return None,
    };
    Some(Call {
        object: header.path()?.as_str().to_owned(),
        method,
        argument,
    })
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
        if own_name {
            connection.request_name(BLUEZ_NAME).await.unwrap();
        }
        ready.send((connection.clone(), Handle::current())).unwrap();
        loop {
            let next = poll_fn(|context| {
                if Pin::new(&mut stopped).poll(context).is_ready() {
                    return Poll::Ready(None);
                }
                Pin::new(&mut calls).poll_next(context).map(Some)
            });
            let call = match next.await {
                None => return,
                Some(Some(Ok(call))) => call,
                Some(Some(Err(error))) => panic!("the fake BlueZ cannot read its bus: {error}"),
                Some(None) => panic!("the fake BlueZ's bus closed before the end of the test"),
            };
            if call.message_type() != Type::MethodCall {
                continue;
            }
            let reply = objects
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .answer(&call);
            if let Some(reply) = reply {
                connection.send(&reply).await.unwrap();
            }
        }
    });
}
