//! Watches BlueZ and tells the driver when paired AirPods appear, connect, disconnect or go.

mod calls;
mod monitor;
mod objects;
#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap};
use std::future::poll_fn;
use std::ops::ControlFlow;
use std::pin::{Pin, pin};
use std::sync::Once;
use std::task::Poll;
use std::time::Duration;

use pods_core::link::Call;
use pods_core::policy::MonitorFact;
use pods_proto::addr::BdAddr;
use tokio::sync::{mpsc, watch};
use tokio::time::{Sleep, sleep, timeout};
use zbus::export::futures_core::Stream;
use zbus::export::ordered_stream::{IntoStream, JoinMultiple, OrderedStreamExt, Peekable};
use zbus::export::serde::Deserialize;
use zbus::message::{Flags, Type as MessageType};
use zbus::names::MemberName;
use zbus::zvariant::Type;
use zbus::{Connection, DBusError, Message, MessageStream, OwnedObjectPath, OwnedVariant};

use super::link::{DeviceEvent, Input, Request};
use super::log;
use calls::Calls;
use monitor::{APP_PATH, MANAGER_INTERFACE, Monitor};
use objects::{Candidates, Change, Properties};

/// Listed by a device that offers the AirPods control channel.
pub(super) const AAP_UUID: &str = "74ec2172-0bad-4d01-8f77-997b2be0722a";

/// The C++ `sweepTimeoutMs`: bluetoothd answers in milliseconds; longer is a wedged BlueZ.
const SWEEP_TIMEOUT_MS: u64 = 2000;

/// A busy or starting bluetoothd answers a later sweep.
const SWEEP_RETRY_MS: u64 = 1000;

pub(super) const SYSTEM_BUS_RETRY_MS: u64 = 5000;

/// Holds no address and no name, and is written once per daemon life.
const NOT_AIRPODS_LOG: &str = "bluez: one device is not AirPods";
static NOT_AIRPODS_LOGGED: Once = Once::new();

const BLUEZ: &str = "org.bluez";
const BUS: &str = "org.freedesktop.DBus";
const DEVICE_INTERFACE: &str = "org.bluez.Device1";
const ADAPTER_INTERFACE: &str = "org.bluez.Adapter1";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

const OWNER_RULE: &str = "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.bluez'";
const OBJECTS_RULE: &str =
    "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.ObjectManager',path='/'";
const PROPERTIES_RULE: &str = "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path_namespace='/org/bluez',arg0='org.bluez.Device1'";
const ADAPTER_RULE: &str = "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path_namespace='/org/bluez',arg0='org.bluez.Adapter1'";

const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";

const CLASSIFYING: [&str; 4] = ["Connected", "UUIDs", "ServicesResolved", "Modalias"];

/// A new path's `Connected` comes before its `UUIDs`, and a kept path's `Modalias` needs a whole read, so these make it worth a read.
const WORTH_A_READ: [&str; 3] = ["UUIDs", "ServicesResolved", "Modalias"];

/// Every match rule's signals, in the order the connection received them.
type Signals = IntoStream<JoinMultiple<Vec<Peekable<MessageStream>>>>;

/// Sample: `{/org/bluez/hci0/dev_02_00_00_00_00_01: {"org.bluez.Device1": {"Connected": <true>}}}`
type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedVariant>>>;

/// A call outlives the connection it went out on and ends by its own timeout.
pub(super) async fn watch(
    inputs: mpsc::Sender<Input>,
    mut requests: mpsc::Receiver<Request>,
    mut monitor_wanted: watch::Receiver<bool>,
) {
    let mut errors = ErrorLog::default();
    let mut calls = Calls::default();
    let mut monitor = Monitor::default();
    loop {
        match Connection::system().await {
            Ok(connection) => {
                errors.clear();
                let mut watcher = Watcher {
                    connection,
                    inputs: &inputs,
                    candidates: Candidates::default(),
                    errors: &mut errors,
                    retry: None,
                    calls: &mut calls,
                    monitor: &mut monitor,
                };
                if watcher
                    .run(&mut requests, &mut monitor_wanted)
                    .await
                    .is_break()
                {
                    return;
                }
            }
            Err(error) => errors.note(format!(
                "bluez: cannot reach the system bus: {}; trying again every {SYSTEM_BUS_RETRY_MS} ms",
                failure(&error)
            )),
        }
        if tell(&inputs, monitor.forget_bluez()).await.is_break()
            || send(&inputs, DeviceEvent::Lost).await.is_break()
            || wait_to_retry(
                &inputs,
                &mut requests,
                &mut monitor_wanted,
                &mut calls,
                &mut monitor,
            )
            .await
            .is_break()
        {
            return;
        }
    }
}

enum Next {
    Signal(Message),
    Request(Request),
    Retry,
    CallEnded(Call, zbus::Result<()>),
    Wanted,
    Monitor(monitor::Event),
    Closed,
    DriverGone,
}

/// One per system bus connection.
struct Watcher<'a> {
    connection: Connection,
    inputs: &'a mpsc::Sender<Input>,
    candidates: Candidates,
    errors: &'a mut ErrorLog,
    /// Only after a sweep failed with BlueZ on the bus.
    retry: Option<Pin<Box<Sleep>>>,
    calls: &'a mut Calls,
    monitor: &'a mut Monitor,
}

impl Watcher<'_> {
    /// Subscribes, then sweeps; `Break` once the driver is gone.
    async fn run(
        &mut self,
        requests: &mut mpsc::Receiver<Request>,
        monitor_wanted: &mut watch::Receiver<bool>,
    ) -> ControlFlow<()> {
        let mut streams = Vec::new();
        let calls_rule = format!("type='method_call',path_namespace='{APP_PATH}'");
        let rules = [
            OWNER_RULE,
            OBJECTS_RULE,
            PROPERTIES_RULE,
            ADAPTER_RULE,
            &calls_rule,
        ];
        for rule in rules {
            match MessageStream::for_match_rule(rule, &self.connection, None).await {
                Ok(stream) => streams.push(stream.peekable()),
                Err(error) => {
                    self.errors.note(format!(
                        "bluez: cannot subscribe to BlueZ: {}",
                        failure(&error)
                    ));
                    return ControlFlow::Continue(());
                }
            }
        }
        let mut signals = JoinMultiple(streams).into_stream();
        self.sweep().await?;
        loop {
            let waits = Waits {
                retry: &mut self.retry,
                calls: self.calls,
                monitor: self.monitor,
                monitor_wanted: &mut *monitor_wanted,
            };
            match next(&mut signals, requests, waits).await {
                Next::Signal(message) => self.take(&message).await?,
                Next::Request(Request::Sweep) | Next::Retry => self.sweep().await?,
                Next::Request(Request::Call {
                    device,
                    call,
                    when_worn,
                }) => {
                    self.call(device, call, when_worn).await?;
                }
                Next::Wanted => {
                    let wanted = *monitor_wanted.borrow_and_update();
                    let fact = self.monitor.want(wanted, &self.connection);
                    tell(self.inputs, fact).await?;
                }
                Next::CallEnded(call, result) => call_ended(self.inputs, call, result).await?,
                Next::Monitor(monitor::Event::Ended(ended)) => {
                    if let Some((adapter, Err(reason))) = &ended {
                        self.errors.note(format!(
                            "bluez: {adapter} refused the advert monitor: {reason}"
                        ));
                    }
                    let fact = self.monitor.ended(ended);
                    tell(self.inputs, fact).await?;
                }
                Next::Monitor(monitor::Event::RetryDue) => self.monitor.retry(&self.connection),
                Next::Monitor(monitor::Event::Read(read)) => {
                    if let Some(sighting) = self.monitor.read_ended(read) {
                        monitor::offer(self.inputs, sighting);
                    }
                }
                Next::Closed => return ControlFlow::Continue(()),
                Next::DriverGone => return ControlFlow::Break(()),
            }
        }
    }

    /// No call goes to a device BlueZ does not hold.
    async fn call(&mut self, device: BdAddr, call: Call, when_worn: bool) -> ControlFlow<()> {
        // A removal taken after the machine chose `device`, before this request, leaves no path.
        let Some(path) = self.candidates.path_of(device) else {
            return not_made(self.inputs, call, "BlueZ holds no such device").await;
        };
        if !self
            .calls
            .start(&self.connection, path.to_owned(), call, when_worn)
        {
            return not_made(self.inputs, call, "too many calls already wait for BlueZ").await;
        }
        ControlFlow::Continue(())
    }

    /// Sample input: `("org.bluez", ":1.4", "")`, `(/org/bluez/hci0/dev_02_00_00_00_00_01, {"org.bluez.Device1": {"Connected": <true>}})` or `(/org/bluez/hci0/dev_02_00_00_00_00_01, ["org.bluez.Device1"])`.
    async fn take(&mut self, message: &Message) -> ControlFlow<()> {
        if message.message_type() == MessageType::MethodCall {
            return self.called(message).await;
        }
        let header = message.header();
        let change = match header.member().map(MemberName::as_str) {
            Some("NameOwnerChanged") => {
                match self.body::<(String, String, String)>(message, "NameOwnerChanged") {
                    Some((_, _, owner)) if owner.is_empty() => {
                        let fact = self.monitor.forget_bluez();
                        tell(self.inputs, fact).await?;
                        Some(Change::Lost)
                    }
                    Some(_) => return self.sweep().await,
                    None => None,
                }
            }
            Some("InterfacesAdded") => {
                let added = self.body::<(
                    OwnedObjectPath,
                    HashMap<String, HashMap<String, OwnedVariant>>,
                )>(message, "InterfacesAdded");
                let Some((path, interfaces)) = added else {
                    return ControlFlow::Continue(());
                };
                if interfaces.contains_key(MANAGER_INTERFACE) {
                    let fact = self.monitor.manager_added(path.as_str(), &self.connection);
                    tell(self.inputs, fact).await?;
                }
                interfaces
                    .get(DEVICE_INTERFACE)
                    .map(|device| Change::Device {
                        path: path.to_string(),
                        properties: Properties::read(device),
                    })
            }
            Some("InterfacesRemoved") => {
                let removed =
                    self.body::<(OwnedObjectPath, Vec<String>)>(message, "InterfacesRemoved");
                let Some((path, interfaces)) = removed else {
                    return ControlFlow::Continue(());
                };
                let names = |wanted: &str| interfaces.iter().any(|name| name == wanted);
                if names(MANAGER_INTERFACE) {
                    let fact = self.monitor.manager_removed(path.as_str());
                    tell(self.inputs, fact).await?;
                }
                names(DEVICE_INTERFACE).then(|| {
                    self.monitor.forget_device(path.as_str());
                    Change::Removed {
                        path: path.to_string(),
                    }
                })
            }
            Some("PropertiesChanged") => match header.path() {
                Some(path) => return self.changed(path.as_str(), message).await,
                None => None,
            },
            _ => None,
        };
        match change {
            Some(change) => self.report(change).await,
            None => ControlFlow::Continue(()),
        }
    }

    /// Sample input: `DeviceFound(/org/bluez/hci0/dev_02_00_00_00_00_01)` on the monitor, or `GetManagedObjects()` on the application.
    async fn called(&mut self, message: &Message) -> ControlFlow<()> {
        let header = message.header();
        let answer = match header.member().map(MemberName::as_str) {
            Some("GetManagedObjects") => Message::method_return(&header).build(&monitor::objects()),
            Some("Release") => {
                let fact = self.monitor.released(&self.connection);
                return tell(self.inputs, fact).await;
            }
            Some("DeviceFound") => {
                if let Some((path,)) = self.body::<(OwnedObjectPath,)>(message, "DeviceFound") {
                    self.monitor.found(path.as_str(), &self.connection);
                }
                return ControlFlow::Continue(());
            }
            Some("Activate") => {
                self.monitor.activated();
                return ControlFlow::Continue(());
            }
            Some("DeviceLost") => return ControlFlow::Continue(()),
            _ if message
                .primary_header()
                .flags()
                .contains(Flags::NO_REPLY_EXPECTED) =>
            {
                return ControlFlow::Continue(());
            }
            _ => Message::error(&header, UNKNOWN_METHOD)
                .build(&("not served by the advert monitor",)),
        };
        if let Ok(answer) = answer {
            // A send that fails is a connection that is closing, which the signal streams report.
            self.connection.send(&answer).await.ok();
        }
        ControlFlow::Continue(())
    }

    /// Sample input: `("org.bluez.Device1", {"Connected": <true>}, ["Alias"])` or `("org.bluez.Adapter1", {"Powered": <true>}, [])`.
    async fn changed(&mut self, path: &str, message: &Message) -> ControlFlow<()> {
        let Some((interface, changed, _)) = self.body::<(
            String,
            HashMap<String, OwnedVariant>,
            Vec<String>,
        )>(message, "PropertiesChanged") else {
            return ControlFlow::Continue(());
        };
        if interface == ADAPTER_INTERFACE {
            let powered = changed
                .get("Powered")
                .and_then(|powered| bool::try_from(&**powered).ok());
            let fact = if powered == Some(true) {
                self.monitor.cause(&self.connection)
            } else {
                None
            };
            return tell(self.inputs, fact).await;
        }
        let advert = changed.get("ManufacturerData");
        if let Some(sighting) = advert.and_then(|data| self.monitor.changed(path, data)) {
            monitor::offer(self.inputs, sighting);
        }
        match self.classify(path, &changed).await {
            Some(properties) => {
                let path = path.to_owned();
                self.report(Change::Device { path, properties }).await
            }
            None => ControlFlow::Continue(()),
        }
    }

    async fn classify(
        &mut self,
        path: &str,
        changed: &HashMap<String, OwnedVariant>,
    ) -> Option<Properties> {
        let names = |list: &[&str]| list.iter().any(|name| changed.contains_key(*name));
        if !names(&CLASSIFYING) {
            return None;
        }
        // A `Modalias` may turn kept pods into Beats, which only a whole read can tell.
        if self.candidates.holds(path) && !changed.contains_key("Modalias") {
            Some(Properties::read(changed))
        } else if names(&WORTH_A_READ) {
            self.read(path).await
        } else {
            None
        }
    }

    async fn read(&mut self, path: &str) -> Option<Properties> {
        let reply = device_properties(&self.connection, path).await;
        let properties = self.noted(reply, "a device")?;
        Some(Properties::read(&properties))
    }

    async fn sweep(&mut self) -> ControlFlow<()> {
        let call = self.connection.call_method(
            Some(BLUEZ),
            "/",
            Some(OBJECT_MANAGER),
            "GetManagedObjects",
            &(),
        );
        let reply = reply::<Managed>(call).await;
        let Some(objects) = self.noted(reply, "BlueZ's devices") else {
            let on_bus = self.bluez_on_bus().await;
            self.retry = on_bus.then(|| Box::pin(sleep(Duration::from_millis(SWEEP_RETRY_MS))));
            return ControlFlow::Continue(());
        };
        self.retry = None;
        self.errors.clear();
        let managers: BTreeSet<String> = objects
            .iter()
            .filter(|(_, interfaces)| interfaces.contains_key(MANAGER_INTERFACE))
            .map(|(path, _)| path.to_string())
            .collect();
        let devices = objects
            .into_iter()
            .filter_map(|(path, interfaces)| {
                let properties = Properties::read(interfaces.get(DEVICE_INTERFACE)?);
                Some((path.to_string(), properties))
            })
            .collect();
        let fact = self.monitor.swept(managers, &self.connection);
        tell(self.inputs, fact).await?;
        self.report(Change::Swept(devices)).await
    }

    /// Reads `(true,)`; without BlueZ no sweep is retried, as its return brings one.
    async fn bluez_on_bus(&self) -> bool {
        let call = self.connection.call_method(
            Some(BUS),
            "/org/freedesktop/DBus",
            Some(BUS),
            "NameHasOwner",
            &(BLUEZ,),
        );
        reply::<bool>(call).await == Ok(true)
    }

    /// On failure logs the signature once, which holds no device data.
    fn body<T>(&mut self, message: &Message, member: &str) -> Option<T>
    where
        T: for<'d> Deserialize<'d> + Type,
    {
        let body = message.body();
        let read = body.deserialize().ok();
        if read.is_none() {
            let signature = body.signature();
            self.errors.note(format!(
                "bluez: cannot read a {member} signal of signature {signature}"
            ));
        }
        read
    }

    fn noted<T>(&mut self, reply: Result<T, String>, what: &str) -> Option<T> {
        match reply {
            Ok(body) => Some(body),
            Err(failure) => {
                self.errors
                    .note(format!("bluez: cannot read {what}: {failure}"));
                None
            }
        }
    }

    async fn report(&mut self, change: Change) -> ControlFlow<()> {
        if change.names_beats() {
            NOT_AIRPODS_LOGGED.call_once(|| log(NOT_AIRPODS_LOG));
        }
        for event in self.candidates.apply(change) {
            send(self.inputs, event).await?;
        }
        ControlFlow::Continue(())
    }
}

async fn reply<T>(call: impl Future<Output = zbus::Result<Message>>) -> Result<T, String>
where
    T: for<'d> Deserialize<'d> + Type,
{
    match timeout(Duration::from_millis(SWEEP_TIMEOUT_MS), call).await {
        Ok(Ok(reply)) => reply
            .body()
            .deserialize()
            .map_err(|error| error.to_string()),
        Ok(Err(error)) => Err(failure(&error)),
        Err(_) => Err(format!("no answer within {SWEEP_TIMEOUT_MS} ms")),
    }
}

/// BlueZ and the bus can include device paths and addresses in error descriptions.
fn failure(error: &zbus::Error) -> String {
    use zbus::Error;

    match error {
        Error::MethodError(name, _, _) => name.to_string(),
        Error::FDO(error) => error.name().to_string(),
        Error::DBus(error) => error.name().to_string(),
        Error::Handshake(_) => "D-Bus handshake failed".to_owned(),
        Error::Failure(_)
        | Error::InputOutput(_)
        | Error::IncorrectType
        | Error::Utf8(_)
        | Error::PaddingNot0(_)
        | Error::UnknownFd
        | Error::SignatureMismatch(_, _)
        | Error::OutOfBounds
        | Error::MaxDepthExceeded(_)
        | Error::SignatureParse(_)
        | Error::EmptyStructure
        | Error::InvalidObjectPath
        | Error::InvalidName(_)
        | Error::InvalidNameConversion { .. }
        | Error::InterfaceNotFound
        | Error::Address(_)
        | Error::InvalidField
        | Error::ExcessData
        | Error::IncorrectEndian
        | Error::InvalidReply
        | Error::MissingField
        | Error::InvalidGUID
        | Error::Unsupported
        | Error::NameTaken
        | Error::InvalidMatchRule
        | Error::MissingParameter(_)
        | Error::InvalidSerial
        | Error::InterfaceExists(_, _)
        | Error::Connection(_, _) => error.to_string(),
        _ => "D-Bus error".to_owned(),
    }
}

async fn call_ended(
    inputs: &mpsc::Sender<Input>,
    call: Call,
    result: zbus::Result<()>,
) -> ControlFlow<()> {
    let Err(error) = result else {
        return ControlFlow::Continue(());
    };
    log(&format!(
        "bluez: {:?} failed: {}",
        call.action,
        failure(&error)
    ));
    send(inputs, DeviceEvent::CallFailed(call)).await
}

async fn not_made(inputs: &mpsc::Sender<Input>, call: Call, reason: &str) -> ControlFlow<()> {
    log(&format!("bluez: {:?} not made: {reason}", call.action));
    send(inputs, DeviceEvent::CallFailed(call)).await
}

/// Every property of a device; the watcher's reads and the monitor's reads share it.
async fn device_properties(
    connection: &Connection,
    path: &str,
) -> Result<HashMap<String, OwnedVariant>, String> {
    let call = connection.call_method(
        Some(BLUEZ),
        path,
        Some(PROPERTIES),
        "GetAll",
        &(DEVICE_INTERFACE,),
    );
    reply(call).await
}

/// What `next` waits on besides the signals and the requests.
struct Waits<'a> {
    retry: &'a mut Option<Pin<Box<Sleep>>>,
    calls: &'a mut Calls,
    monitor: &'a mut Monitor,
    monitor_wanted: &'a mut watch::Receiver<bool>,
}

async fn next(
    signals: &mut Signals,
    requests: &mut mpsc::Receiver<Request>,
    waits: Waits<'_>,
) -> Next {
    let Waits {
        retry,
        calls,
        monitor,
        monitor_wanted,
    } = waits;
    let mut new_want = pin!(monitor_wanted.changed());
    poll_fn(|context| {
        match Pin::new(&mut *signals).poll_next(context) {
            Poll::Ready(Some(Ok(message))) => return Poll::Ready(Next::Signal(message)),
            // A read error ends the connection, and its streams end after it.
            Poll::Ready(Some(Err(_)) | None) => return Poll::Ready(Next::Closed),
            Poll::Pending => {}
        }
        if let Poll::Ready((call, result)) = calls.poll_ended(context) {
            return Poll::Ready(Next::CallEnded(call, result));
        }
        if let Poll::Ready(event) = monitor.poll(context) {
            return Poll::Ready(Next::Monitor(event));
        }
        // The driver drops the sender only as it ends, with the request queue.
        if let Poll::Ready(changed) = new_want.as_mut().poll(context) {
            return Poll::Ready(changed.map_or(Next::DriverGone, |()| Next::Wanted));
        }
        if let Poll::Ready(request) = requests.poll_recv(context) {
            return Poll::Ready(request.map_or(Next::DriverGone, Next::Request));
        }
        let waited = retry
            .as_mut()
            .is_some_and(|wait| wait.as_mut().poll(context).is_ready());
        if !waited {
            return Poll::Pending;
        }
        *retry = None;
        Poll::Ready(Next::Retry)
    })
    .await
}

enum Retry {
    Due,
    Request(Request),
    Wanted,
    CallEnded(Call, zbus::Result<()>),
    DriverGone,
}

/// Fails every new call meanwhile; `Break` once the driver is gone.
async fn wait_to_retry(
    inputs: &mpsc::Sender<Input>,
    requests: &mut mpsc::Receiver<Request>,
    monitor_wanted: &mut watch::Receiver<bool>,
    calls: &mut Calls,
    monitor: &mut Monitor,
) -> ControlFlow<()> {
    let mut pause = pin!(sleep(Duration::from_millis(SYSTEM_BUS_RETRY_MS)));
    loop {
        let woke = {
            let mut new_want = pin!(monitor_wanted.changed());
            poll_fn(|context| {
                if pause.as_mut().poll(context).is_ready() {
                    return Poll::Ready(Retry::Due);
                }
                if let Poll::Ready((call, result)) = calls.poll_ended(context) {
                    return Poll::Ready(Retry::CallEnded(call, result));
                }
                if let Poll::Ready(changed) = new_want.as_mut().poll(context) {
                    return Poll::Ready(changed.map_or(Retry::DriverGone, |()| Retry::Wanted));
                }
                requests.poll_recv(context).map(|request| match request {
                    Some(request) => Retry::Request(request),
                    None => Retry::DriverGone,
                })
            })
            .await
        };
        match woke {
            Retry::Due => return ControlFlow::Continue(()),
            Retry::DriverGone => return ControlFlow::Break(()),
            Retry::Request(Request::Sweep) => {}
            Retry::Wanted => monitor.set_wanted(*monitor_wanted.borrow_and_update()),
            Retry::Request(Request::Call { call, .. }) => {
                not_made(inputs, call, "the system bus is out of reach").await?;
            }
            Retry::CallEnded(call, result) => call_ended(inputs, call, result).await?,
        }
    }
}

/// Hands the driver what the monitor reads now, if that changed; `Break` once the driver is gone.
async fn tell(inputs: &mpsc::Sender<Input>, fact: Option<MonitorFact>) -> ControlFlow<()> {
    let Some(fact) = fact else {
        return ControlFlow::Continue(());
    };
    match inputs.send(Input::Monitor(fact)).await {
        Ok(()) => ControlFlow::Continue(()),
        Err(_) => ControlFlow::Break(()),
    }
}

/// `Break` once the driver is gone.
async fn send(inputs: &mpsc::Sender<Input>, event: DeviceEvent) -> ControlFlow<()> {
    match inputs.send(Input::Device(event)).await {
        Ok(()) => ControlFlow::Continue(()),
        Err(_) => ControlFlow::Break(()),
    }
}

/// A repeat is not logged until a success clears it, as the C++ `m_lastSweepError`.
#[derive(Debug, Default)]
pub(super) struct ErrorLog {
    last: Option<String>,
}

impl ErrorLog {
    pub(super) fn note(&mut self, line: String) {
        if self.last.as_ref() != Some(&line) {
            log(&line);
            self.last = Some(line);
        }
    }

    pub(super) fn clear(&mut self) {
        self.last = None;
    }
}
