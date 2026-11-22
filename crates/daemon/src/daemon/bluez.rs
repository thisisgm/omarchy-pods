//! Watches BlueZ and tells the driver when paired AirPods appear, connect, disconnect or go.

mod calls;
mod objects;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::future::poll_fn;
use std::ops::ControlFlow;
use std::pin::{Pin, pin};
use std::sync::Once;
use std::task::Poll;
use std::time::Duration;

use pods_core::link::Call;
use pods_proto::addr::BdAddr;
use tokio::sync::mpsc;
use tokio::time::{Sleep, sleep, timeout};
use zbus::export::futures_core::Stream;
use zbus::export::ordered_stream::{IntoStream, JoinMultiple, OrderedStreamExt, Peekable};
use zbus::export::serde::Deserialize;
use zbus::names::MemberName;
use zbus::zvariant::Type;
use zbus::{Connection, Message, MessageStream, OwnedObjectPath, OwnedVariant};

use super::link::{DeviceEvent, Input, Request};
use super::log;
use calls::Calls;
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
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

const OWNER_RULE: &str = "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.bluez'";
const OBJECTS_RULE: &str =
    "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.ObjectManager',path='/'";
const PROPERTIES_RULE: &str = "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path_namespace='/org/bluez',arg0='org.bluez.Device1'";

const CLASSIFYING: [&str; 4] = ["Connected", "UUIDs", "ServicesResolved", "Modalias"];

/// A new path's `Connected` comes before its `UUIDs`, and a kept path's `Modalias` needs a whole read, so these make it worth a read.
const WORTH_A_READ: [&str; 3] = ["UUIDs", "ServicesResolved", "Modalias"];

/// Every match rule's signals, in the order the connection received them.
type Signals = IntoStream<JoinMultiple<Vec<Peekable<MessageStream>>>>;

/// Sample: `{/org/bluez/hci0/dev_02_00_00_00_00_01: {"org.bluez.Device1": {"Connected": <true>}}}`
type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedVariant>>>;

/// A call outlives the connection it went out on and ends by its own timeout.
pub(super) async fn watch(inputs: mpsc::Sender<Input>, mut requests: mpsc::Receiver<Request>) {
    let mut errors = ErrorLog::default();
    let mut calls = Calls::default();
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
                };
                if watcher.run(&mut requests).await.is_break() {
                    return;
                }
            }
            Err(error) => errors.note(format!(
                "bluez: cannot reach the system bus: {error}; trying again every {SYSTEM_BUS_RETRY_MS} ms"
            )),
        }
        if send(&inputs, DeviceEvent::Lost).await.is_break()
            || wait_to_retry(&inputs, &mut requests, &mut calls)
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
}

impl Watcher<'_> {
    /// Subscribes, then sweeps; `Break` once the driver is gone.
    async fn run(&mut self, requests: &mut mpsc::Receiver<Request>) -> ControlFlow<()> {
        let mut streams = Vec::new();
        for rule in [OWNER_RULE, OBJECTS_RULE, PROPERTIES_RULE] {
            match MessageStream::for_match_rule(rule, &self.connection, None).await {
                Ok(stream) => streams.push(stream.peekable()),
                Err(error) => {
                    self.errors
                        .note(format!("bluez: cannot subscribe to BlueZ: {error}"));
                    return ControlFlow::Continue(());
                }
            }
        }
        let mut signals = JoinMultiple(streams).into_stream();
        self.sweep().await?;
        loop {
            match next(&mut signals, requests, &mut self.retry, self.calls).await {
                Next::Signal(message) => self.take(&message).await?,
                Next::Request(Request::Sweep) | Next::Retry => self.sweep().await?,
                Next::Request(Request::Call { device, call }) => {
                    self.call(device, call).await?;
                }
                Next::CallEnded(call, result) => call_ended(self.inputs, call, result).await?,
                Next::Closed => return ControlFlow::Continue(()),
                Next::DriverGone => return ControlFlow::Break(()),
            }
        }
    }

    /// No call goes to a device BlueZ does not hold.
    async fn call(&mut self, device: BdAddr, call: Call) -> ControlFlow<()> {
        // A removal taken after the machine chose `device`, before this request, leaves no path.
        let Some(path) = self.candidates.path_of(device) else {
            return not_made(self.inputs, call, "BlueZ holds no such device").await;
        };
        if !self.calls.start(&self.connection, path.to_owned(), call) {
            return not_made(self.inputs, call, "too many calls already wait for BlueZ").await;
        }
        ControlFlow::Continue(())
    }

    /// Sample input: `("org.bluez", ":1.4", "")`, `(/org/bluez/hci0/dev_02_00_00_00_00_01, {"org.bluez.Device1": {"Connected": <true>}})` or `(/org/bluez/hci0/dev_02_00_00_00_00_01, ["org.bluez.Device1"])`.
    async fn take(&mut self, message: &Message) -> ControlFlow<()> {
        let header = message.header();
        let change = match header.member().map(MemberName::as_str) {
            Some("NameOwnerChanged") => {
                match self.body::<(String, String, String)>(message, "NameOwnerChanged") {
                    Some((_, _, owner)) if owner.is_empty() => Some(Change::Lost),
                    Some(_) => return self.sweep().await,
                    None => None,
                }
            }
            Some("InterfacesAdded") => self
                .body::<(
                    OwnedObjectPath,
                    HashMap<String, HashMap<String, OwnedVariant>>,
                )>(message, "InterfacesAdded")
                .and_then(|(path, interfaces)| {
                    let properties = Properties::read(interfaces.get(DEVICE_INTERFACE)?);
                    Some(Change::Device {
                        path: path.to_string(),
                        properties,
                    })
                }),
            Some("InterfacesRemoved") => self
                .body::<(OwnedObjectPath, Vec<String>)>(message, "InterfacesRemoved")
                .filter(|(_, interfaces)| interfaces.iter().any(|name| name == DEVICE_INTERFACE))
                .map(|(path, _)| Change::Removed {
                    path: path.to_string(),
                }),
            Some("PropertiesChanged") => match header.path() {
                Some(path) => self.changed(path.as_str(), message).await,
                None => None,
            },
            _ => None,
        };
        match change {
            Some(change) => self.report(change).await,
            None => ControlFlow::Continue(()),
        }
    }

    /// Sample input: `("org.bluez.Device1", {"Connected": <true>}, ["Alias"])`.
    async fn changed(&mut self, path: &str, message: &Message) -> Option<Change> {
        let (_, changed, _): (String, HashMap<String, OwnedVariant>, Vec<String>) =
            self.body(message, "PropertiesChanged")?;
        let names = |list: &[&str]| list.iter().any(|name| changed.contains_key(*name));
        if !names(&CLASSIFYING) {
            return None;
        }
        // A `Modalias` may turn kept pods into Beats, which only a whole read can tell.
        let properties = if self.candidates.holds(path) && !changed.contains_key("Modalias") {
            Properties::read(&changed)
        } else if names(&WORTH_A_READ) {
            self.read(path).await?
        } else {
            return None;
        };
        Some(Change::Device {
            path: path.to_owned(),
            properties,
        })
    }

    async fn read(&mut self, path: &str) -> Option<Properties> {
        let call = self.connection.call_method(
            Some(BLUEZ),
            path,
            Some(PROPERTIES),
            "GetAll",
            &(DEVICE_INTERFACE,),
        );
        let reply = reply::<HashMap<String, OwnedVariant>>(call).await;
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
        let devices = objects
            .into_iter()
            .filter_map(|(path, interfaces)| {
                let properties = Properties::read(interfaces.get(DEVICE_INTERFACE)?);
                Some((path.to_string(), properties))
            })
            .collect();
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

/// A method error logs its name alone: BlueZ may put the path, so the address, in the message.
fn failure(error: &zbus::Error) -> String {
    if let zbus::Error::MethodError(name, _, _) = error {
        name.to_string()
    } else {
        error.to_string()
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

async fn next(
    signals: &mut Signals,
    requests: &mut mpsc::Receiver<Request>,
    retry: &mut Option<Pin<Box<Sleep>>>,
    calls: &mut Calls,
) -> Next {
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
    CallEnded(Call, zbus::Result<()>),
    DriverGone,
}

/// Fails every new call meanwhile; `Break` once the driver is gone.
async fn wait_to_retry(
    inputs: &mpsc::Sender<Input>,
    requests: &mut mpsc::Receiver<Request>,
    calls: &mut Calls,
) -> ControlFlow<()> {
    let mut pause = pin!(sleep(Duration::from_millis(SYSTEM_BUS_RETRY_MS)));
    loop {
        let woke = poll_fn(|context| {
            if pause.as_mut().poll(context).is_ready() {
                return Poll::Ready(Retry::Due);
            }
            if let Poll::Ready((call, result)) = calls.poll_ended(context) {
                return Poll::Ready(Retry::CallEnded(call, result));
            }
            requests.poll_recv(context).map(|request| match request {
                Some(request) => Retry::Request(request),
                None => Retry::DriverGone,
            })
        })
        .await;
        match woke {
            Retry::Due => return ControlFlow::Continue(()),
            Retry::DriverGone => return ControlFlow::Break(()),
            Retry::Request(Request::Sweep) => {}
            Retry::Request(Request::Call { call, .. }) => {
                not_made(inputs, call, "the system bus is out of reach").await?;
            }
            Retry::CallEnded(call, result) => call_ended(inputs, call, result).await?,
        }
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
