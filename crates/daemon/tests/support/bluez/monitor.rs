//! The fake BlueZ's `org.bluez.AdvertisementMonitorManager1`, after `src/adv_monitor.c` of BlueZ 5.87: registrations read through the application's object manager, and the devices adverts make.

use std::collections::HashMap;
use std::mem;
use std::num::NonZeroU32;
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::time::timeout;
use zbus::message::Flags;
use zbus::{Connection, Message, OwnedObjectPath, OwnedVariant, Str, Variant};

use super::super::wait_within;
use super::{
    AdapterPath, Answer, Bluez, DEVICE_INTERFACE, FAILED, Managed, OBJECT_MANAGER, Objects,
    PROPERTIES, UNKNOWN, object,
};

pub(super) const MANAGER_INTERFACE: &str = "org.bluez.AdvertisementMonitorManager1";
const MONITOR_INTERFACE: &str = "org.bluez.AdvertisementMonitor1";
const OR_PATTERNS: &str = "or_patterns";
const ALREADY_EXISTS: &str = "org.bluez.Error.AlreadyExists";
const DOES_NOT_EXIST: &str = "org.bluez.Error.DoesNotExist";
const INVALID_ARGUMENTS: &str = "org.bluez.Error.InvalidArguments";
/// `BT_AD_MAX_DATA_LEN` of BlueZ's `src/shared/ad.h`: no pattern reaches past it.
const AD_MAX_DATA_LEN: usize = 31;
/// The types `ad_is_type_valid` of BlueZ's `src/shared/ad.c` takes besides manufacturer data: flags to 3D information.
const AD_TYPES: RangeInclusive<u8> = 0x01..=0x3d;
const AD_MANUFACTURER_DATA: u8 = 0xff;
/// Apple's company identifier, which BlueZ puts little endian before the data it matches.
pub(crate) const APPLE: u16 = 0x004c;
/// Any of these on a monitor makes BlueZ report a device once and later its loss, not every advert.
const RSSI_PROPERTIES: [&str; 5] = [
    "RSSILowThreshold",
    "RSSIHighThreshold",
    "RSSILowTimeout",
    "RSSIHighTimeout",
    "RSSISamplingPeriod",
];
/// The strength every advert of the fake arrives with.
const ADVERT_RSSI: i16 = -50;
/// How long a registration waits for the application's object manager before it fails; inside the test's own wait.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// A monitor BlueZ accepted, as it read it: `or_patterns` and patterns of start, AD type and content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Registered {
    pub(crate) kind: String,
    pub(crate) patterns: Vec<(u8, u8, Vec<u8>)>,
}

/// An address in range of every adapter; `lose` takes it out of range, dropping it changes nothing.
pub(crate) struct Advertiser {
    bluez: Bluez,
    address: String,
}

#[derive(Default)]
pub(super) struct Monitors {
    /// The adapters that carry the manager interface.
    managers: Vec<String>,
    refuse: bool,
    apps: Vec<App>,
    devices: Vec<Advertised>,
}

/// What BlueZ tells applications apart by: the adapter, the owner's unique name and the root path.
#[derive(Clone, PartialEq, Eq)]
struct Key {
    adapter: String,
    owner: String,
    path: String,
}

/// A registered application, its monitors empty until BlueZ has read them.
struct App {
    key: Key,
    /// The serial of its `RegisterMonitor` call, which tells a later registration under the same key apart.
    serial: NonZeroU32,
    monitors: Vec<Monitor>,
}

struct Monitor {
    path: String,
    registered: Registered,
    /// Set once its `Activate` is sent; until then it is not listed and hears no advert.
    active: bool,
    tracking: bool,
    /// The devices a tracking monitor was told of and not yet of their loss.
    found: Vec<String>,
}

/// A device BlueZ made for an advert a monitor matched.
struct Advertised {
    path: String,
    adapter: String,
    address: String,
    data: Vec<u8>,
}

/// A registration accepted and not yet answered, which the serving thread finishes by reading the application.
pub(super) struct Registration {
    call: Message,
    key: Key,
}

impl Bluez {
    /// Serves the manager interface on the adapter, as BlueZ does with `Experimental` set.
    pub(crate) fn add_monitor_manager(&self, adapter: &AdapterPath) {
        self.lock().monitors.managers.push(adapter.0.clone());
        let interfaces = HashMap::from([(MANAGER_INTERFACE.to_owned(), manager_properties())]);
        let body = (object(&adapter.0), interfaces);
        self.emit("/", OBJECT_MANAGER, "InterfacesAdded", &body);
    }

    /// Takes the manager interface off the adapter and releases every monitor registered there.
    pub(crate) fn remove_monitor_manager(&self, adapter: &AdapterPath) {
        let releases = self.lock().monitors.drop_adapter(&adapter.0);
        self.send(releases);
        let body = (object(&adapter.0), vec![MANAGER_INTERFACE]);
        self.emit("/", OBJECT_MANAGER, "InterfacesRemoved", &body);
    }

    /// While set, `RegisterMonitor` answers `org.bluez.Error.Failed`, as BlueZ does when it cannot make the application.
    pub(crate) fn refuse_monitors(&self, refuse: bool) {
        self.lock().monitors.refuse = refuse;
    }

    /// Every monitor whose `Activate` was sent, read through its application's object manager, in the order they were registered.
    pub(crate) fn monitors(&self) -> Vec<Registered> {
        let objects = self.lock();
        let apps = objects.monitors.apps.iter();
        let monitors = apps.flat_map(|app| &app.monitors);
        let active = monitors.filter(|monitor| monitor.active);
        active.map(|monitor| monitor.registered.clone()).collect()
    }

    pub(crate) fn wait_monitors(&self, count: usize, within: Duration) -> Vec<Registered> {
        wait_within(&format!("{count} registered monitors"), within, || {
            let monitors = self.monitors();
            (monitors.len() == count).then_some(monitors)
        })
    }

    /// Puts `address` in range sending `apple_data`, the manufacturer data that follows Apple's company identifier.
    pub(crate) fn advertise(&self, address: &str, apple_data: &[u8]) -> Advertiser {
        let advertiser = Advertiser {
            bluez: self.clone(),
            address: address.to_owned(),
        };
        advertiser.update(apple_data);
        advertiser
    }

    /// Calls `Release` on every monitor and drops it while its application stays registered, as when the kernel removes the monitors.
    pub(crate) fn release_monitors(&self) {
        let releases = self.lock().monitors.release_all();
        self.send(releases);
    }
}

impl Advertiser {
    /// The next advert: each powered adapter with a monitor it matches shows the device, then tells those monitors.
    pub(crate) fn update(&self, apple_data: &[u8]) {
        let messages = {
            let mut objects = self.bluez.lock();
            let suffix = device_path("", &self.address);
            assert!(
                !objects.devices.keys().any(|path| path.ends_with(&suffix)),
                "the fake cannot advertise from the address of a device added with add_device"
            );
            let powered = objects.adapters.iter().filter(|adapter| adapter.powered);
            let listening: Vec<String> = powered.map(|adapter| adapter.path.clone()).collect();
            objects
                .monitors
                .deliver(&listening, &self.address, apple_data)
        };
        self.bluez.send(messages);
    }

    /// Out of range: tracking monitors hear `DeviceLost`, then the device goes.
    pub(crate) fn lose(self) {
        let messages = self.bluez.lock().monitors.lose(&self.address);
        self.bluez.send(messages);
    }
}

impl Monitors {
    pub(super) fn serves(&self, adapter: &str) -> bool {
        self.managers.iter().any(|path| path == adapter)
    }

    /// The manager interface with its properties, when the adapter carries it.
    pub(super) fn manager_of(
        &self,
        adapter: &str,
    ) -> Option<(String, HashMap<String, OwnedVariant>)> {
        let interface = (MANAGER_INTERFACE.to_owned(), manager_properties());
        self.serves(adapter).then_some(interface)
    }

    pub(super) fn managed_devices(&self) -> Managed {
        let devices = self.devices.iter().map(|device| {
            let interfaces = HashMap::from([(DEVICE_INTERFACE.to_owned(), device.properties())]);
            (object(&device.path).into(), interfaces)
        });
        devices.collect()
    }

    pub(super) fn device_properties(&self, path: &str) -> Option<HashMap<String, OwnedVariant>> {
        let device = self.devices.iter().find(|device| device.path == path)?;
        Some(device.properties())
    }

    /// The answer to `RegisterMonitor` or `UnregisterMonitor` on `adapter`, in the order BlueZ checks.
    pub(super) fn answer(&mut self, call: &Message, adapter: &str, member: &str) -> Answer {
        let header = call.header();
        let owner = header.sender().map(ToString::to_string);
        let path = call.body().deserialize::<OwnedObjectPath>();
        let (Some(owner), Ok(path)) = (owner, path) else {
            return refusal(call, INVALID_ARGUMENTS);
        };
        let key = Key {
            adapter: adapter.to_owned(),
            owner,
            path: path.to_string(),
        };
        let known = self.apps.iter().position(|app| app.key == key);
        match (member, known) {
            ("RegisterMonitor", Some(_)) => refusal(call, ALREADY_EXISTS),
            ("RegisterMonitor", None) if self.refuse => refusal(call, FAILED),
            ("RegisterMonitor", None) => {
                self.apps.push(App {
                    key: key.clone(),
                    serial: call.primary_header().serial_num(),
                    monitors: Vec::new(),
                });
                Answer::Register(Registration {
                    call: call.clone(),
                    key,
                })
            }
            ("UnregisterMonitor", Some(index)) => {
                let app = self.apps.remove(index);
                let mut messages = releases(&app);
                messages.push(Message::method_return(&header).build(&()).unwrap());
                Answer::Send(messages)
            }
            ("UnregisterMonitor", None) => refusal(call, DOES_NOT_EXIST),
            _ => refusal(call, UNKNOWN),
        }
    }

    /// Ends every registration `owner` made, with no `Release`, as BlueZ does when the owner leaves the bus.
    pub(super) fn forget(&mut self, owner: &str) {
        self.apps.retain(|app| app.key.owner != owner);
    }

    /// `DeviceLost` for every device a tracking monitor on the adapter was told of, as BlueZ clears its timers at power down.
    pub(super) fn power_down(&mut self, adapter: &str) -> Vec<Message> {
        self.lost(adapter, |_| true)
    }

    fn release_all(&mut self) -> Vec<Message> {
        let mut messages = Vec::new();
        for app in &mut self.apps {
            messages.extend(releases(app));
            app.monitors.clear();
        }
        messages
    }

    fn drop_adapter(&mut self, adapter: &str) -> Vec<Message> {
        self.managers.retain(|path| path != adapter);
        let (gone, kept) = mem::take(&mut self.apps)
            .into_iter()
            .partition::<Vec<_>, _>(|app| app.key.adapter == adapter);
        self.apps = kept;
        gone.iter().flat_map(releases).collect()
    }

    /// The application this registration made, unless it has since ended.
    fn registered(&self, registration: &Registration) -> Option<usize> {
        let serial = registration.call.primary_header().serial_num();
        let made = |app: &App| app.key == registration.key && app.serial == serial;
        self.apps.iter().position(made)
    }

    /// Releases the invalid monitors read, answers the registration, then activates the valid ones.
    fn complete(&mut self, registration: &Registration, read: Option<Managed>) -> Vec<Message> {
        let Registration { call, key } = registration;
        let header = call.header();
        let Some(index) = self.registered(registration) else {
            return vec![error(call, FAILED)];
        };
        let Some(read) = read else {
            self.apps.remove(index);
            return vec![error(call, FAILED)];
        };
        let app = &mut self.apps[index];
        let mut objects: Vec<(String, HashMap<String, OwnedVariant>)> = read
            .into_iter()
            .filter(|(object, _)| object.as_str().starts_with(&key.path))
            .filter_map(|(object, mut interfaces)| {
                Some((object.to_string(), interfaces.remove(MONITOR_INTERFACE)?))
            })
            .collect();
        objects.sort_by(|left, right| left.0.cmp(&right.0));
        let mut messages = Vec::new();
        for (object, properties) in objects {
            match Monitor::read(&object, &properties) {
                Some(monitor) => app.monitors.push(monitor),
                None => messages.push(notice(&key.owner, &object, "Release", None)),
            }
        }
        messages.push(Message::method_return(&header).build(&()).unwrap());
        let activations = app.monitors.iter();
        messages
            .extend(activations.map(|monitor| notice(&key.owner, &monitor.path, "Activate", None)));
        messages
    }

    /// For each listening adapter carrying the manager: the device shown, then `DeviceFound` to every monitor the advert matches.
    fn deliver(&mut self, listening: &[String], address: &str, data: &[u8]) -> Vec<Message> {
        let mut ad = APPLE.to_le_bytes().to_vec();
        ad.extend_from_slice(data);
        let adapters = self.managers.iter().filter(|path| listening.contains(path));
        let adapters: Vec<String> = adapters.cloned().collect();
        let mut messages = Vec::new();
        for adapter in adapters {
            let device = device_path(&adapter, address);
            let mut found = Vec::new();
            let mut matched = false;
            for app in self
                .apps
                .iter_mut()
                .filter(|app| app.key.adapter == adapter)
            {
                for monitor in app
                    .monitors
                    .iter_mut()
                    .filter(|monitor| monitor.active && monitor.matches(&ad))
                {
                    matched = true;
                    if monitor.tracking {
                        if monitor.found.contains(&device) {
                            continue;
                        }
                        monitor.found.push(device.clone());
                    }
                    found.push(notice(
                        &app.key.owner,
                        &monitor.path,
                        "DeviceFound",
                        Some(&device),
                    ));
                }
            }
            if matched {
                messages.extend(self.show(&adapter, &device, address, data));
                messages.extend(found);
            }
        }
        messages
    }

    /// The signal that shows the device with `data`: added when new, changed when the data differs, none when it is the same.
    fn show(&mut self, adapter: &str, device: &str, address: &str, data: &[u8]) -> Option<Message> {
        let signal = match self.devices.iter_mut().find(|known| known.path == device) {
            Some(known) if known.data == data => return None,
            Some(known) => {
                known.data = data.to_vec();
                let changed = HashMap::from([("ManufacturerData", manufacturer_data(data))]);
                let body = (DEVICE_INTERFACE, changed, Vec::<String>::new());
                Message::signal(device, PROPERTIES, "PropertiesChanged").build(&body)
            }
            None => {
                let advertised = Advertised {
                    path: device.to_owned(),
                    adapter: adapter.to_owned(),
                    address: address.to_owned(),
                    data: data.to_vec(),
                };
                let interfaces = HashMap::from([(DEVICE_INTERFACE, advertised.properties())]);
                self.devices.push(advertised);
                let body = (object(device), interfaces);
                Message::signal("/", OBJECT_MANAGER, "InterfacesAdded").build(&body)
            }
        };
        Some(signal.unwrap())
    }

    /// `DeviceLost` to the tracking monitors told of the address's devices, then each device removed.
    fn lose(&mut self, address: &str) -> Vec<Message> {
        let (gone, kept) = mem::take(&mut self.devices)
            .into_iter()
            .partition::<Vec<_>, _>(|device| device.address == address);
        self.devices = kept;
        let mut messages = Vec::new();
        for device in gone {
            messages.extend(self.lost(&device.adapter, |found| found == device.path));
            let body = (object(&device.path), vec![DEVICE_INTERFACE]);
            let removed = Message::signal("/", OBJECT_MANAGER, "InterfacesRemoved").build(&body);
            messages.push(removed.unwrap());
        }
        messages
    }

    /// `DeviceLost` to each tracking monitor on the adapter for every device it was told of that `gone` picks, which it then forgets.
    fn lost(&mut self, adapter: &str, gone: impl Fn(&str) -> bool) -> Vec<Message> {
        let mut messages = Vec::new();
        for app in self
            .apps
            .iter_mut()
            .filter(|app| app.key.adapter == adapter)
        {
            for monitor in &mut app.monitors {
                for device in monitor.found.iter().filter(|device| gone(device)) {
                    messages.push(notice(
                        &app.key.owner,
                        &monitor.path,
                        "DeviceLost",
                        Some(device),
                    ));
                }
                monitor.found.retain(|device| !gone(device));
            }
        }
        messages
    }
}

impl Monitor {
    /// The monitor at `path` when its properties pass `monitor_process` of BlueZ: type `or_patterns`, at least one valid pattern.
    fn read(path: &str, properties: &HashMap<String, OwnedVariant>) -> Option<Self> {
        let kind = String::try_from(properties.get("Type")?.try_clone().ok()?).ok()?;
        let patterns: Vec<(u8, u8, Vec<u8>)> =
            Vec::try_from(properties.get("Patterns")?.try_clone().ok()?).ok()?;
        if kind != OR_PATTERNS || patterns.is_empty() || !patterns.iter().all(valid) {
            return None;
        }
        Some(Self {
            path: path.to_owned(),
            registered: Registered { kind, patterns },
            active: false,
            tracking: RSSI_PROPERTIES
                .iter()
                .any(|name| properties.contains_key(*name)),
            found: Vec::new(),
        })
    }

    /// Whether a pattern matches the manufacturer data `ad`, company identifier first, as `match_manufacturer` compares.
    fn matches(&self, ad: &[u8]) -> bool {
        let patterns = self.registered.patterns.iter();
        patterns.into_iter().any(|(start, ad_type, content)| {
            let start = usize::from(*start);
            let end = start.saturating_add(content.len());
            *ad_type == AD_MANUFACTURER_DATA && ad.get(start..end) == Some(content.as_slice())
        })
    }
}

impl Advertised {
    fn properties(&self) -> HashMap<String, OwnedVariant> {
        let text = |value: String| OwnedVariant::from(Str::from(value));
        HashMap::from([
            ("Address".to_owned(), text(self.address.clone())),
            ("AddressType".to_owned(), text("random".to_owned())),
            ("Alias".to_owned(), text(self.address.replace(':', "-"))),
            (
                "Adapter".to_owned(),
                OwnedVariant::from(object(&self.adapter)),
            ),
            ("Paired".to_owned(), OwnedVariant::from(false)),
            ("Connected".to_owned(), OwnedVariant::from(false)),
            ("RSSI".to_owned(), OwnedVariant::from(ADVERT_RSSI)),
            ("ManufacturerData".to_owned(), manufacturer_data(&self.data)),
        ])
    }
}

/// Reads the application's monitors through its object manager, as BlueZ's client does, then answers the registration.
pub(super) async fn register(
    connection: Connection,
    objects: Arc<Mutex<Objects>>,
    registration: Registration,
) {
    let read = connection.call_method(
        Some(registration.key.owner.as_str()),
        registration.key.path.as_str(),
        Some(OBJECT_MANAGER),
        "GetManagedObjects",
        &(),
    );
    let managed = match timeout(READ_TIMEOUT, read).await {
        Ok(Ok(reply)) => reply.body().deserialize::<Managed>().ok(),
        Ok(Err(_)) | Err(_) => None,
    };
    let lock = || objects.lock().unwrap_or_else(PoisonError::into_inner);
    let messages = lock().monitors.complete(&registration, managed);
    for message in messages {
        connection.send(&message).await.unwrap();
    }
    let mut state = lock();
    if let Some(index) = state.monitors.registered(&registration) {
        let app = &mut state.monitors.apps[index];
        app.monitors
            .iter_mut()
            .for_each(|monitor| monitor.active = true);
    }
}

/// Whether `bt_ad_pattern_new` of BlueZ takes the pattern: content of 1 to 31 bytes inside 31, a known AD type.
fn valid((start, ad_type, content): &(u8, u8, Vec<u8>)) -> bool {
    let start = usize::from(*start);
    let known = AD_TYPES.contains(ad_type) || *ad_type == AD_MANUFACTURER_DATA;
    let inside = start.saturating_add(content.len()) <= AD_MAX_DATA_LEN;
    known && !content.is_empty() && start < AD_MAX_DATA_LEN && inside
}

/// The `Release` BlueZ sends each monitor of an application it ends.
fn releases(app: &App) -> Vec<Message> {
    let monitors = app.monitors.iter();
    monitors
        .map(|monitor| notice(&app.key.owner, &monitor.path, "Release", None))
        .collect()
}

/// A call on a monitor object that wants no reply, as BlueZ sends `Activate`, `Release`, `DeviceFound` and `DeviceLost`.
fn notice(owner: &str, monitor: &str, member: &str, device: Option<&str>) -> Message {
    let call = Message::method_call(monitor, member)
        .destination(owner)
        .interface(MONITOR_INTERFACE)
        .with_flags(Flags::NO_REPLY_EXPECTED);
    match device {
        Some(device) => call.build(&(object(device),)),
        None => call.build(&()),
    }
    .unwrap()
}

fn error(call: &Message, name: &str) -> Message {
    Message::error(&call.header(), name)
        .build(&(name,))
        .unwrap()
}

fn refusal(call: &Message, name: &str) -> Answer {
    Answer::Send(vec![error(call, name)])
}

fn manager_properties() -> HashMap<String, OwnedVariant> {
    let types = OwnedVariant::try_from(Variant::from(vec![OR_PATTERNS])).unwrap();
    let features = OwnedVariant::try_from(Variant::from(Vec::<String>::new())).unwrap();
    HashMap::from([
        ("SupportedMonitorTypes".to_owned(), types),
        ("SupportedFeatures".to_owned(), features),
    ])
}

/// `ManufacturerData` as BlueZ writes it, `a{qv}` with the data under Apple's company identifier.
fn manufacturer_data(data: &[u8]) -> OwnedVariant {
    let entries = HashMap::from([(APPLE, Variant::from(data.to_vec()))]);
    OwnedVariant::try_from(Variant::from(entries)).unwrap()
}

/// The path BlueZ gives the address's device on the adapter: `/org/bluez/hci0/dev_02_00_00_00_00_01`.
fn device_path(adapter: &str, address: &str) -> String {
    format!("{adapter}/dev_{}", address.to_uppercase().replace(':', "_"))
}
