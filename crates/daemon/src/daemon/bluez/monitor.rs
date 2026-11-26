//! The one BlueZ advert monitor: registered on every adapter that offers advert monitors while the policy wants it, its adverts handed to the driver.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use pods_core::policy::{MonitorFact, Sighting};
use pods_proto::addr::BdAddr;
use pods_proto::proximity::{ADVERT_LEN, APPLE_COMPANY_ID, MANUFACTURER_DATA, PROXIMITY_TYPE};
use tokio::sync::{Barrier, mpsc};
use tokio::task::JoinSet;
use tokio::time::{Sleep, sleep, timeout};
use zbus::{Array, Connection, Dict, ObjectPath, Variant};

use super::{BLUEZ, device_properties, failure};
use crate::daemon::link::Input;

/// A full queue drops the advert without a log line.
pub(super) const ADVERT_QUEUE: usize = 16;

/// Paths the monitor reported; the oldest leaves when a new one arrives.
pub(super) const ADVERT_DEVICES_MAX: usize = 32;

/// A refused registration is tried again after this, at most `MONITOR_TRIES` times per cause.
pub(super) const MONITOR_RETRY_MS: u64 = 1000;

/// After this many refusals the intake reads `unavailable` until the next cause, so a BlueZ without monitors costs no timer.
pub(super) const MONITOR_TRIES: u32 = 5;

pub(super) const MANAGER_INTERFACE: &str = "org.bluez.AdvertisementMonitorManager1";

const MONITOR_INTERFACE: &str = "org.bluez.AdvertisementMonitor1";

/// The application BlueZ reads the monitor through, and the one monitor under it.
pub(super) const APP_PATH: &str = "/librepods/adverts";

const MONITOR_PATH: &str = "/librepods/adverts/apple";

const OR_PATTERNS: &str = "or_patterns";

const APPLE_LOW_HIGH: [u8; 2] = APPLE_COMPANY_ID.to_le_bytes();

/// The one pattern: from the start of manufacturer data, Apple's company identifier little endian, then the proximity type.
const PATTERN: (u8, u8, [u8; 3]) = (
    0,
    MANUFACTURER_DATA,
    [APPLE_LOW_HIGH[0], APPLE_LOW_HIGH[1], PROXIMITY_TYPE],
);

/// The BlueZ `RegisterMonitor` answers once it has read the application, which takes milliseconds; longer is a wedged BlueZ.
const MANAGER_CALL_TIMEOUT_MS: u64 = 5000;

/// The body of the `GetManagedObjects` reply BlueZ reads: path, then interface, then property.
pub(super) type Objects =
    HashMap<ObjectPath<'static>, HashMap<&'static str, HashMap<&'static str, Variant<'static>>>>;

/// What a call task ends with: an adapter and how BlueZ answered its registration, or `None` for an unregistration.
type Ended = Option<(String, Result<(), String>)>;

/// What the first read of a reported path ends with: the path, then its address and its advert, or `None` when the read failed.
type Read = (String, Option<(BdAddr, Option<[u8; ADVERT_LEN]>)>);

/// Outlives a system bus connection, so what the policy wants stays across a reconnect.
#[derive(Debug, Default)]
pub(super) struct Monitor {
    wanted: bool,
    /// The adapters that carry the manager interface.
    managers: BTreeSet<String>,
    calls: JoinSet<Ended>,
    /// The first reads of reported paths, one per path in `reading`; a task, so the watcher keeps draining the bus meanwhile.
    reads: JoinSet<Read>,
    /// The adapters of the try under way that refused.
    refused: BTreeSet<String>,
    /// Some adapter accepted since the last cause.
    accepted: bool,
    /// An `Activate` came since the last try began, so BlueZ has answered a registration of this try.
    activated: bool,
    /// Tries since the last cause that ended with a refusal.
    refusals: u32,
    retry: Option<Pin<Box<Sleep>>>,
    /// The device paths the monitor reported, with their addresses, oldest first.
    reported: VecDeque<(String, BdAddr)>,
    /// The paths under a first read, at most `ADVERT_QUEUE`, each with its latest change, kept over the reply since BlueZ signals again any value newer than that change.
    reading: Vec<(String, Option<[u8; ADVERT_LEN]>)>,
}

pub(super) enum Event {
    Ended(Ended),
    RetryDue,
    /// `None` for a read that panicked, which leaves its path under read until the next stop.
    Read(Option<Read>),
}

impl Monitor {
    /// With no connection: the next connection's sweep registers the monitor if it is wanted.
    pub(super) fn set_wanted(&mut self, wanted: bool) {
        self.wanted = wanted;
    }

    pub(super) fn want(&mut self, wanted: bool, connection: &Connection) -> Option<MonitorFact> {
        self.wanted = wanted;
        if wanted {
            return self.cause(connection);
        }
        self.stop_calls();
        for adapter in &self.managers {
            let task = unregister(connection.clone(), adapter.clone());
            self.calls.spawn(async move {
                // An unregistration nobody waits on: a refusal means there was nothing to drop.
                task.await.ok();
                None
            });
        }
        None
    }

    /// While wanted, a reason to register again: the first try at once, a refused one again every `MONITOR_RETRY_MS`.
    pub(super) fn cause(&mut self, connection: &Connection) -> Option<MonitorFact> {
        if !self.wanted {
            return None;
        }
        let was_observing = self.accepted || self.activated;
        self.stop_calls();
        self.refusals = 0;
        if self.managers.is_empty() {
            return Some(MonitorFact::Unavailable);
        }
        self.attempt(connection, self.managers.clone());
        was_observing.then_some(MonitorFact::Interrupted)
    }

    /// bluetoothd or the connection to it is gone, so nothing it held stands; `unavailable` while wanted if it offered monitors.
    pub(super) fn forget_bluez(&mut self) -> Option<MonitorFact> {
        let offered = !self.managers.is_empty();
        self.stop_calls();
        self.managers.clear();
        (self.wanted && offered).then_some(MonitorFact::Unavailable)
    }

    /// From a sweep; an adapter offering monitors that was not known is a cause.
    pub(super) fn swept(
        &mut self,
        managers: BTreeSet<String>,
        connection: &Connection,
    ) -> Option<MonitorFact> {
        let appeared = managers.difference(&self.managers).next().is_some();
        self.managers = managers;
        if appeared {
            self.cause(connection)
        } else if self.wanted && self.managers.is_empty() {
            Some(MonitorFact::Unavailable)
        } else {
            None
        }
    }

    pub(super) fn manager_added(
        &mut self,
        adapter: &str,
        connection: &Connection,
    ) -> Option<MonitorFact> {
        self.managers.insert(adapter.to_owned());
        self.cause(connection)
    }

    pub(super) fn manager_removed(&mut self, adapter: &str) -> Option<MonitorFact> {
        self.managers.remove(adapter);
        (self.wanted && self.managers.is_empty()).then_some(MonitorFact::Unavailable)
    }

    pub(super) fn activated(&mut self) {
        self.activated = true;
    }

    /// A `Release` after an `Activate` or while no call is under way is BlueZ dropping the monitor on its own, a cause.
    pub(super) fn released(&mut self, connection: &Connection) -> Option<MonitorFact> {
        if self.activated || self.calls.is_empty() {
            self.cause(connection)
        } else {
            None
        }
    }

    /// Reads a path a `DeviceFound` names the first time; nothing is read while the monitor is not wanted or `ADVERT_QUEUE` reads are under way.
    pub(super) fn found(&mut self, path: &str, connection: &Connection) {
        if !self.begin_read(path) {
            return;
        }
        let connection = connection.clone();
        let path = path.to_owned();
        self.reads
            .spawn(async move { read(connection, path).await });
    }

    /// Marks `path` under read and says so, unless it is reported, already under read, or `ADVERT_QUEUE` paths are.
    fn begin_read(&mut self, path: &str) -> bool {
        let known = self.reported.iter().any(|(known, _)| known == path);
        let under_read = self.reading.iter().any(|(reading, _)| reading == path);
        if !self.wanted || known || under_read || self.reading.len() >= ADVERT_QUEUE {
            return false;
        }
        self.reading.push((path.to_owned(), None));
        true
    }

    /// The advert of a `ManufacturerData` change: given at once for a reported path, kept until its read ends for a path under one.
    pub(super) fn changed(
        &mut self,
        path: &str,
        manufacturer_data: &Variant<'_>,
    ) -> Option<Sighting> {
        let data = apple_advert(manufacturer_data)?;
        if let Some(address) = self.address_of(path) {
            return Some(Sighting { address, data });
        }
        let under_read = self.reading.iter_mut().find(|(reading, _)| reading == path);
        if let Some((_, latest)) = under_read {
            *latest = Some(data);
        }
        None
    }

    /// Remembers the path a read ended on and gives its advert, a change kept meanwhile first; a read that failed is dropped unlogged.
    pub(super) fn read_ended(&mut self, read: Option<Read>) -> Option<Sighting> {
        let (path, read) = read?;
        let at = self
            .reading
            .iter()
            .position(|(reading, _)| *reading == path);
        let changed_data = at.and_then(|at| self.reading.swap_remove(at).1);
        let (address, read_data) = read?;
        if !self.wanted {
            return None;
        }
        self.remember(path, address);
        let data = changed_data.or(read_data)?;
        Some(Sighting { address, data })
    }

    /// Remembers a reported path once its address is read, once.
    fn remember(&mut self, path: String, address: BdAddr) {
        if self.reported.iter().any(|(known, _)| *known == path) {
            return;
        }
        if self.reported.len() >= ADVERT_DEVICES_MAX {
            self.reported.pop_front();
        }
        self.reported.push_back((path, address));
    }

    /// The address of a path the monitor reported.
    fn address_of(&self, path: &str) -> Option<BdAddr> {
        let known = self.reported.iter().find(|(known, _)| known == path);
        known.map(|(_, address)| *address)
    }

    pub(super) fn forget_device(&mut self, path: &str) {
        self.reported.retain(|(known, _)| known != path);
    }

    /// `Ended` once a call ends, `Read` once a read ends, `RetryDue` once a refused try may run again; `Pending` while none.
    pub(super) fn poll(&mut self, context: &mut Context<'_>) -> Poll<Event> {
        if let Poll::Ready(Some(joined)) = self.calls.poll_join_next(context) {
            // A panicked call ends with no answer, as an unregistration does.
            return Poll::Ready(Event::Ended(joined.unwrap_or(None)));
        }
        if let Poll::Ready(Some(joined)) = self.reads.poll_join_next(context) {
            return Poll::Ready(Event::Read(joined.ok()));
        }
        let due = self
            .retry
            .as_mut()
            .is_some_and(|wait| wait.as_mut().poll(context).is_ready());
        if !due {
            return Poll::Pending;
        }
        self.retry = None;
        Poll::Ready(Event::RetryDue)
    }

    pub(super) fn retry(&mut self, connection: &Connection) {
        let refused = std::mem::take(&mut self.refused);
        self.attempt(connection, refused);
    }

    /// Counts a try that ended refused once its last call ends; after `MONITOR_TRIES` the intake reads `unavailable` unless an adapter accepted.
    pub(super) fn ended(&mut self, ended: Ended) -> Option<MonitorFact> {
        let registered = match ended {
            Some((_, Ok(()))) => {
                self.accepted = true;
                self.wanted.then_some(MonitorFact::Registered)
            }
            Some((adapter, Err(_))) => {
                self.refused.insert(adapter);
                None
            }
            None => None,
        };
        if !self.wanted || !self.calls.is_empty() || self.refused.is_empty() {
            return registered;
        }
        self.refusals = self.refusals.saturating_add(1);
        if self.refusals < MONITOR_TRIES {
            self.retry = Some(Box::pin(sleep(Duration::from_millis(MONITOR_RETRY_MS))));
            return registered;
        }
        self.refused.clear();
        if self.accepted {
            registered
        } else {
            Some(MonitorFact::Unavailable)
        }
    }

    fn attempt(&mut self, connection: &Connection, adapters: BTreeSet<String>) {
        self.activated = false;
        let unregistered = Arc::new(Barrier::new(adapters.len()));
        for adapter in adapters {
            let connection = connection.clone();
            let unregistered = Arc::clone(&unregistered);
            self.calls.spawn(async move {
                let answer = register(connection, &adapter, &unregistered).await;
                Some((adapter, answer))
            });
        }
    }

    /// Aborted calls that BlueZ still runs are undone by the next registration's unregistration first.
    fn stop_calls(&mut self) {
        // A new set, since an aborted set still yields a task that ended before the abort; dropping the old one aborts the rest.
        self.calls = JoinSet::new();
        self.reads = JoinSet::new();
        self.reading.clear();
        self.retry = None;
        self.refused.clear();
        self.reported.clear();
        self.accepted = false;
        self.activated = false;
    }
}

/// What BlueZ reads through the application root: the one monitor with its type and pattern.
pub(super) fn objects() -> Objects {
    let (start, ad_type, content) = PATTERN;
    let properties = HashMap::from([
        ("Type", Variant::from(OR_PATTERNS)),
        (
            "Patterns",
            Variant::from(vec![(start, ad_type, content.to_vec())]),
        ),
    ]);
    let monitor = HashMap::from([(MONITOR_INTERFACE, properties)]);
    HashMap::from([(ObjectPath::from_static_str_unchecked(MONITOR_PATH), monitor)])
}

/// Sample input: `{0x004c: <[0x07, 0x19, 0x01, 0x27, 0x20, ...]>}`, `ManufacturerData` as `a{qv}`; a shorter Apple entry or none is dropped.
fn apple_advert(manufacturer_data: &Variant<'_>) -> Option<[u8; ADVERT_LEN]> {
    let entries = <&Dict<'_, '_>>::try_from(manufacturer_data).ok()?;
    let bytes: &Array<'_> = entries.get(&APPLE_COMPANY_ID).ok()??;
    let mut data = [0_u8; ADVERT_LEN];
    let mut taken = 0_usize;
    for (slot, byte) in data.iter_mut().zip(bytes.iter()) {
        *slot = u8::try_from(byte).ok()?;
        taken = taken.saturating_add(1);
    }
    (taken == ADVERT_LEN).then_some(data)
}

/// Sample input: `{"Address": <"02:00:00:00:00:01">, "ManufacturerData": <{0x004c: <[0x07, ...]>}>, ...}`, the reply of `GetAll` on `Device1`.
async fn read(connection: Connection, path: String) -> Read {
    let read = async {
        let properties = device_properties(&connection, &path).await.ok()?;
        let address: BdAddr = <&str>::try_from(&**properties.get("Address")?)
            .ok()?
            .parse()
            .ok()?;
        let advert = properties
            .get("ManufacturerData")
            .and_then(|data| apple_advert(data));
        Some((address, advert))
    };
    let read = read.await;
    (path, read)
}

/// Queued only while fewer than `ADVERT_QUEUE` inputs wait, so adverts never crowd a command out; otherwise dropped.
pub(super) fn offer(inputs: &mpsc::Sender<Input>, sighting: Sighting) {
    let waiting = inputs.max_capacity().saturating_sub(inputs.capacity());
    if waiting < ADVERT_QUEUE {
        // Full or closed: the advert is dropped, as the bound says.
        inputs.try_send(Input::Advert(sighting)).ok();
    }
}

/// `UnregisterMonitor` first, so a registration BlueZ still holds from before is dropped and read anew; no adapter of the try registers until every one has unregistered, so their `Release`s all come before any `Activate`.
async fn register(
    connection: Connection,
    adapter: &str,
    unregistered: &Barrier,
) -> Result<(), String> {
    unregister(connection.clone(), adapter.to_owned())
        .await
        .ok();
    unregistered.wait().await;
    manager_call(&connection, adapter, "RegisterMonitor").await
}

async fn unregister(connection: Connection, adapter: String) -> Result<(), String> {
    manager_call(&connection, &adapter, "UnregisterMonitor").await
}

async fn manager_call(connection: &Connection, adapter: &str, method: &str) -> Result<(), String> {
    let app = ObjectPath::from_static_str_unchecked(APP_PATH);
    let body = (app,);
    let call = connection.call_method(Some(BLUEZ), adapter, Some(MANAGER_INTERFACE), method, &body);
    match timeout(Duration::from_millis(MANAGER_CALL_TIMEOUT_MS), call).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(failure(&error)),
        Err(_) => Err(format!("no answer within {MANAGER_CALL_TIMEOUT_MS} ms")),
    }
}

#[cfg(test)]
mod tests;
