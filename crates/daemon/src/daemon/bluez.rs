//! Watches BlueZ on the system bus and tells the driver when paired AirPods appear, connect, disconnect or go.

mod calls;
mod objects;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::future::poll_fn;
use std::ops::ControlFlow;
use std::pin::{Pin, pin};
use std::task::Poll;
use std::time::Duration;

use pods_core::link::DeviceAction;
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

/// The service UUID a device lists when it offers the AirPods control channel.
pub(super) const AAP_UUID: &str = "74ec2172-0bad-4d01-8f77-997b2be0722a";

/// The C++ `sweepTimeoutMs`: bluetoothd answers in milliseconds, so a longer wait is a wedged BlueZ.
const SWEEP_TIMEOUT_MS: u64 = 2000;

/// The wait before sweeping again after a sweep that failed with BlueZ on the bus, as a busy or starting bluetoothd answers a later one.
const SWEEP_RETRY_MS: u64 = 1000;

/// The wait before connecting again after the system bus was missing, the connection closed or a subscription failed.
const SYSTEM_BUS_RETRY_MS: u64 = 5000;

const BLUEZ: &str = "org.bluez";
const BUS: &str = "org.freedesktop.DBus";
const DEVICE_INTERFACE: &str = "org.bluez.Device1";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

const OWNER_RULE: &str = "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.bluez'";
const OBJECTS_RULE: &str =
    "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.ObjectManager',path='/'";
const PROPERTIES_RULE: &str = "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path_namespace='/org/bluez',arg0='org.bluez.Device1'";

/// The device properties whose change classifies a device again.
const CLASSIFYING: [&str; 3] = ["Connected", "UUIDs", "ServicesResolved"];

/// The changes that make a path not kept yet worth reading: its `Connected` comes before its `UUIDs`.
const WORTH_A_READ: [&str; 2] = ["UUIDs", "ServicesResolved"];

/// The signals of every match rule merged in the order the connection received them.
type Signals = IntoStream<JoinMultiple<Vec<Peekable<MessageStream>>>>;

/// The body of `GetManagedObjects`, path then interface then property: `{/org/bluez/hci0/dev_02_00_00_00_00_01: {"org.bluez.Device1": {"Connected": <true>}}}`.
type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedVariant>>>;

/// Watches until aborted, or until the driver is gone; a call outlives the connection it went out on and ends by its own timeout.
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

/// What woke the watcher.
enum Next {
    Signal(Message),
    Request(Request),
    /// The wait after a failed sweep is over.
    Retry,
    CallEnded(DeviceAction, zbus::Result<()>),
    /// The connection to the system bus closed.
    Closed,
    /// The driver dropped its end of the request queue.
    DriverGone,
}

/// One connection to the system bus and the devices seen on it.
struct Watcher<'a> {
    connection: Connection,
    inputs: &'a mpsc::Sender<Input>,
    candidates: Candidates,
    errors: &'a mut ErrorLog,
    /// Set while a sweep that failed with BlueZ on the bus waits to be tried again.
    retry: Option<Pin<Box<Sleep>>>,
    calls: &'a mut Calls,
}

impl Watcher<'_> {
    /// Subscribes, sweeps, then follows BlueZ until the connection closes; `Break` once the driver is gone.
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
                Next::Request(Request::Call { device, action }) => {
                    self.call(device, action).await?;
                }
                Next::CallEnded(action, result) => call_ended(self.inputs, action, result).await?,
                Next::Closed => return ControlFlow::Continue(()),
                Next::DriverGone => return ControlFlow::Break(()),
            }
        }
    }

    /// Starts `action` on a path BlueZ holds for `device`, or fails it at once: no call goes to a device BlueZ does not hold.
    async fn call(&mut self, device: BdAddr, action: DeviceAction) -> ControlFlow<()> {
        // A removal taken after the machine chose `device` and before this request leaves no path.
        let Some(path) = self.candidates.path_of(device) else {
            return not_made(self.inputs, action, "BlueZ holds no such device").await;
        };
        if !self.calls.start(&self.connection, path.to_owned(), action) {
            return not_made(self.inputs, action, "too many calls already wait for BlueZ").await;
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

    /// A `PropertiesChanged` of `Device1` such as `("org.bluez.Device1", {"Connected": <true>}, ["Alias"])`, read whole when it may make a candidate of a path not kept.
    async fn changed(&mut self, path: &str, message: &Message) -> Option<Change> {
        let (_, changed, _): (String, HashMap<String, OwnedVariant>, Vec<String>) =
            self.body(message, "PropertiesChanged")?;
        let names = |list: &[&str]| list.iter().any(|name| changed.contains_key(*name));
        if !names(&CLASSIFYING) {
            return None;
        }
        let properties = if self.candidates.holds(path) {
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

    /// Whether `NameHasOwner` answers `(true,)` for BlueZ; without BlueZ on the bus no sweep is tried again, as its return brings one.
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

    /// The body of the signal `member`, or `None` with its signature logged once: a signature holds no device data.
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

    /// The reply, or `None` with the failure logged once.
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
        for event in self.candidates.apply(change) {
            send(self.inputs, event).await?;
        }
        ControlFlow::Continue(())
    }
}

/// The reply body of `call` within `SWEEP_TIMEOUT_MS`, or the failure as a line for the log.
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

/// A failed call as a line for the log: of a method error the name alone, since BlueZ may put the object path, and with it the address, in the message.
fn failure(error: &zbus::Error) -> String {
    if let zbus::Error::MethodError(name, _, _) = error {
        name.to_string()
    } else {
        error.to_string()
    }
}

/// Logs a call that failed and tells the driver; `Break` once the driver is gone.
async fn call_ended(
    inputs: &mpsc::Sender<Input>,
    action: DeviceAction,
    result: zbus::Result<()>,
) -> ControlFlow<()> {
    let Err(error) = result else {
        return ControlFlow::Continue(());
    };
    log(&format!("bluez: {action:?} failed: {}", failure(&error)));
    send(inputs, DeviceEvent::CallFailed(action)).await
}

/// Logs a call that was not made, and why, then tells the driver it failed; `Break` once the driver is gone.
async fn not_made(
    inputs: &mpsc::Sender<Input>,
    action: DeviceAction,
    reason: &str,
) -> ControlFlow<()> {
    log(&format!("bluez: {action:?} not made: {reason}"));
    send(inputs, DeviceEvent::CallFailed(action)).await
}

/// The next signal, call that ended, request, or the end of the wait to sweep again.
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
        if let Poll::Ready((action, result)) = calls.poll_ended(context) {
            return Poll::Ready(Next::CallEnded(action, result));
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

/// What woke a watcher waiting to reach the system bus again.
enum Retry {
    Due,
    Request(Request),
    CallEnded(DeviceAction, zbus::Result<()>),
    DriverGone,
}

/// Waits `SYSTEM_BUS_RETRY_MS`, failing every new call meanwhile; `Break` once the driver is gone.
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
            if let Poll::Ready((action, result)) = calls.poll_ended(context) {
                return Poll::Ready(Retry::CallEnded(action, result));
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
            Retry::Request(Request::Call { action, .. }) => {
                not_made(inputs, action, "the system bus is out of reach").await?;
            }
            Retry::CallEnded(action, result) => call_ended(inputs, action, result).await?,
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

/// The last failure logged, so a repeat is not logged again until a success clears it, as the C++ `m_lastSweepError`.
#[derive(Debug, Default)]
struct ErrorLog {
    last: Option<String>,
}

impl ErrorLog {
    fn note(&mut self, line: String) {
        if self.last.as_ref() != Some(&line) {
            log(&line);
            self.last = Some(line);
        }
    }

    fn clear(&mut self) {
        self.last = None;
    }
}
