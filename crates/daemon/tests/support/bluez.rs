//! A fake BlueZ on a private bus: the object manager at `/`, adapters and devices, served from one thread of its own.

use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pods_proto::addr::BdAddr;
use tokio::runtime::{Builder as RuntimeBuilder, Handle};
use tokio::sync::oneshot;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdapterPath(pub(crate) String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DevicePath(pub(crate) String);

/// The serving thread and the sender that ends it.
type Server = (oneshot::Sender<()>, JoinHandle<()>);

pub(crate) struct FakeBluez {
    connection: Connection,
    handle: Handle,
    objects: Arc<Mutex<Objects>>,
    server: Option<Server>,
}

#[derive(Default)]
struct Objects {
    adapters: Vec<String>,
    devices: HashMap<String, Device>,
    /// Every change of a device object's connection, its adding and its removal included, oldest first.
    edges: Vec<Edge>,
    hold: bool,
    fail: bool,
    /// The `GetManagedObjects` calls that reached the fake, held ones included.
    sweeps: usize,
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
            connection,
            handle,
            objects,
            server: Some((stop, thread)),
        }
    }

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

    /// How many sweeps reached the fake so far.
    pub(crate) fn sweeps(&self) -> usize {
        self.lock().sweeps
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

    /// Whether the serving thread has ended on its own.
    pub(crate) fn stopped_serving(&self) -> bool {
        self.server
            .as_ref()
            .is_none_or(|(_, thread)| thread.is_finished())
    }

    /// Ends the serving thread, failing the test if it stopped serving or panicked before.
    pub(crate) fn finish(mut self) {
        let (stop, thread) = self.server.take().expect("a serving fake BlueZ");
        assert!(
            !thread.is_finished(),
            "the fake BlueZ stopped serving before the end of the test"
        );
        // A thread gone by now ended in a panic, which the join reports.
        stop.send(()).ok();
        assert!(thread.join().is_ok(), "the fake BlueZ panicked");
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
        if let Some((stop, thread)) = self.server.take() {
            // A thread that already ended dropped its receiver, which is the end wanted.
            stop.send(()).ok();
            // A thread that panicked has nothing left to clean up.
            drop(thread.join());
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

    /// The reply to `call`, or none while replies are held.
    fn answer(&mut self, call: &Message) -> Option<Message> {
        let header = call.header();
        if header.member().map(MemberName::as_str) == Some("GetManagedObjects") {
            self.sweeps = self.sweeps.saturating_add(1);
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
            (Some(PROPERTIES), Some("GetAll"), Some(path)) => match self.devices.get(path) {
                Some(device) => Message::method_return(&header).build(&device.properties()),
                None => Message::error(&header, UNKNOWN).build(&("No such object",)),
            },
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
