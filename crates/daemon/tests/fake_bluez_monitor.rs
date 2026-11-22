//! Support tests: the fake BlueZ's advertisement monitor manager, driven by a monitor application the test serves itself.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests drive the fake BlueZ alone, so the daemon helpers the other suites share go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::future::poll_fn;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
    use zbus::connection::Builder;
    use zbus::export::futures_core::Stream;
    use zbus::message::Type;
    use zbus::names::InterfaceName;
    use zbus::{Connection, DBusError, MessageStream, OwnedObjectPath, OwnedVariant, Str, Variant};

    use super::support::bluez::monitor::{APPLE, Registered};
    use super::support::bluez::{AdapterPath, Call, DeviceMethod, FakeBluez};
    use super::support::bus::{Bus, in_time, start_bus};
    use super::support::{HANG_LIMIT, Session};

    const APP_PATH: &str = "/org/example/adverts";
    const MONITOR_PATH: &str = "/org/example/adverts/apple";
    const TRACKING_PATH: &str = "/org/example/adverts/tracking";
    const BLUEZ: &str = "org.bluez";
    const MANAGER: &str = "org.bluez.AdvertisementMonitorManager1";
    const MONITOR: &str = "org.bluez.AdvertisementMonitor1";
    const ADAPTER: &str = "org.bluez.Adapter1";
    const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
    const OR_PATTERNS: &str = "or_patterns";
    const AD_MANUFACTURER_DATA: u8 = 0xff;
    /// Apple's company identifier as BlueZ matches it, little endian, then the proximity type `07`.
    const APPLE_PROXIMITY: [u8; 3] = {
        let [low, high] = APPLE.to_le_bytes();
        [low, high, PROXIMITY[0]]
    };
    /// An Apple advert of the proximity kind, `07 19` then 25 synthetic bytes.
    const PROXIMITY: [u8; 27] = [
        0x07, 0x19, 0x01, 0x27, 0x20, 0x21, 0x88, 0x8f, 0x11, 0x00, 0x04, 0, 1, 2, 3, 4, 5, 6, 7,
        8, 9, 10, 11, 12, 13, 14, 15,
    ];
    /// An Apple advert of the nearby kind, type `10`, which the proximity pattern must not match.
    const NEARBY: [u8; 7] = [0x10, 0x05, 0x01, 0x18, 0x00, 0x00, 0x00];
    /// `data[6]`, the pods battery byte `onDeviceDiscovered` reads.
    const BATTERY_BYTE: usize = 6;
    const NEXT_BATTERY: u8 = 0x55;
    const FIRST: &str = "02:00:00:00:00:01";
    const SECOND: &str = "02:00:00:00:00:02";
    const FIRST_ON_HCI0: &str = "/org/bluez/hci0/dev_02_00_00_00_00_01";
    const SECOND_ON_HCI0: &str = "/org/bluez/hci0/dev_02_00_00_00_00_02";
    const ALREADY_EXISTS: &str = "org.bluez.Error.AlreadyExists";
    const FAILED: &str = "org.bluez.Error.Failed";
    const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";
    /// Present on a monitor, it makes BlueZ report a device once and its loss later.
    const RSSI_LOW_TIMEOUT: &str = "RSSILowTimeout";
    const LOW_TIMEOUT_SECONDS: u16 = 5;

    type Pattern = (u8, u8, Vec<u8>);
    type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedVariant>>>;

    fn apple_pattern() -> Pattern {
        (0, AD_MANUFACTURER_DATA, APPLE_PROXIMITY.to_vec())
    }

    fn apple_monitor() -> Registered {
        Registered {
            kind: OR_PATTERNS.to_owned(),
            patterns: vec![apple_pattern()],
        }
    }

    /// One monitor object the application lists.
    struct Spec {
        path: &'static str,
        kind: &'static str,
        patterns: Vec<Pattern>,
        tracking: bool,
    }

    fn apple_spec() -> Spec {
        Spec {
            path: MONITOR_PATH,
            kind: OR_PATTERNS,
            patterns: vec![apple_pattern()],
            tracking: false,
        }
    }

    /// The object manager at the application's root, which BlueZ reads its monitors through.
    struct Application {
        monitors: Vec<Spec>,
        reads: Arc<AtomicUsize>,
    }

    #[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
    impl Application {
        fn get_managed_objects(&self) -> Managed {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.monitors
                .iter()
                .map(|spec| {
                    let mut properties = HashMap::from([
                        ("Type".to_owned(), OwnedVariant::from(Str::from(spec.kind))),
                        (
                            "Patterns".to_owned(),
                            OwnedVariant::try_from(Variant::from(spec.patterns.clone())).unwrap(),
                        ),
                    ]);
                    if spec.tracking {
                        properties.insert(
                            RSSI_LOW_TIMEOUT.to_owned(),
                            OwnedVariant::from(LOW_TIMEOUT_SECONDS),
                        );
                    }
                    let path = OwnedObjectPath::try_from(spec.path).unwrap();
                    (path, HashMap::from([(MONITOR.to_owned(), properties)]))
                })
                .collect()
        }
    }

    /// A call BlueZ made on a monitor object: the method, the monitor and the device it names.
    #[derive(Debug, PartialEq, Eq)]
    struct Heard {
        member: String,
        monitor: String,
        device: Option<String>,
    }

    fn heard(member: &str, monitor: &str, device: Option<&str>) -> Heard {
        Heard {
            member: member.to_owned(),
            monitor: monitor.to_owned(),
            device: device.map(str::to_owned),
        }
    }

    /// `DeviceFound` on the Apple monitor for `device`.
    fn found(device: &str) -> Heard {
        heard("DeviceFound", MONITOR_PATH, Some(device))
    }

    /// How BlueZ answered a manager call: accepted, or refused with this error name.
    #[derive(Debug, PartialEq, Eq)]
    enum Reply {
        Accepted,
        Refused(String),
    }

    fn refused(name: &str) -> Reply {
        Reply::Refused(name.to_owned())
    }

    /// A client of the private system bus serving `Application` at `APP_PATH`, every message it receives kept in order.
    struct App {
        client: Connection,
        received: MessageStream,
        reads: Arc<AtomicUsize>,
        runtime: Runtime,
    }

    impl App {
        fn serve(address: &str, monitors: Vec<Spec>) -> Self {
            let runtime = RuntimeBuilder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let reads = Arc::new(AtomicUsize::new(0));
            let application = Application {
                monitors,
                reads: Arc::clone(&reads),
            };
            let client = in_time(&runtime, async {
                Builder::address(address)
                    .serve_at(APP_PATH, application)
                    .build()
                    .await
                    .unwrap()
            });
            // The stream exists before any registration, so no call BlueZ makes can pass it unseen.
            let received = MessageStream::from(&client);
            Self {
                client,
                received,
                reads,
                runtime,
            }
        }

        fn register(&self, adapter: &AdapterPath) -> Reply {
            self.manager_call(adapter, "RegisterMonitor")
        }

        fn unregister(&self, adapter: &AdapterPath) -> Reply {
            self.manager_call(adapter, "UnregisterMonitor")
        }

        fn manager_call(&self, adapter: &AdapterPath, method: &str) -> Reply {
            let body = (OwnedObjectPath::try_from(APP_PATH).unwrap(),);
            let call = self.client.call_method(
                Some(BLUEZ),
                adapter.0.as_str(),
                Some(MANAGER),
                method,
                &body,
            );
            match in_time(&self.runtime, call) {
                Ok(_) => Reply::Accepted,
                Err(zbus::Error::MethodError(name, _, _)) => Reply::Refused(name.to_string()),
                Err(zbus::Error::FDO(error)) => Reply::Refused(error.name().to_string()),
                Err(error) => panic!("{method} failed outside BlueZ: {error}"),
            }
        }

        /// The next call BlueZ made on a monitor object, failing once `HANG_LIMIT` passes.
        fn next(&mut self) -> Heard {
            let received = &mut self.received;
            in_time(&self.runtime, async {
                loop {
                    let next = poll_fn(|context| Pin::new(&mut *received).poll_next(context));
                    let message = next.await.expect("the application's stream ended").unwrap();
                    let header = message.header();
                    if message.message_type() != Type::MethodCall
                        || header.interface().map(InterfaceName::as_str) != Some(MONITOR)
                    {
                        continue;
                    }
                    let device = message
                        .body()
                        .deserialize::<OwnedObjectPath>()
                        .ok()
                        .map(|path| path.as_str().to_owned());
                    return Heard {
                        member: header.member().unwrap().to_string(),
                        monitor: header.path().unwrap().to_string(),
                        device,
                    };
                }
            })
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }

        /// A call on BlueZ as a daemon would make it, answered in time.
        fn call<B>(&self, path: &str, interface: &str, method: &str, body: &B)
        where
            B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
        {
            let call = self
                .client
                .call_method(Some(BLUEZ), path, Some(interface), method, body);
            in_time(&self.runtime, call).unwrap();
        }

        fn managed(&self) -> Managed {
            let object_manager = Some("org.freedesktop.DBus.ObjectManager");
            let call =
                self.client
                    .call_method(Some(BLUEZ), "/", object_manager, "GetManagedObjects", &());
            in_time(&self.runtime, call)
                .unwrap()
                .body()
                .deserialize()
                .unwrap()
        }

        fn has_object(&self, path: &str) -> bool {
            self.managed()
                .contains_key(&OwnedObjectPath::try_from(path).unwrap())
        }

        fn manufacturer_data(&self, device: &str) -> Vec<u8> {
            let path = OwnedObjectPath::try_from(device).unwrap();
            let entry = &self.managed()[&path]["org.bluez.Device1"]["ManufacturerData"];
            let mut data: HashMap<u16, OwnedVariant> =
                HashMap::try_from(entry.try_clone().unwrap()).unwrap();
            Vec::try_from(data.remove(&APPLE).unwrap()).unwrap()
        }
    }

    /// The fake BlueZ on its own system bus, one adapter carrying the monitor manager.
    struct Bench {
        bluez: FakeBluez,
        adapter: AdapterPath,
        bus: Bus,
        _session: Session,
    }

    impl Bench {
        fn new() -> Self {
            let session = Session::new();
            let bus = start_bus(&session, "system");
            let bluez = FakeBluez::start(&bus.address, true);
            let adapter = bluez.add_adapter();
            bluez.add_monitor_manager(&adapter);
            Self {
                bluez,
                adapter,
                bus,
                _session: session,
            }
        }

        fn app(&self, monitors: Vec<Spec>) -> App {
            App::serve(&self.bus.address, monitors)
        }

        /// An application with the Apple monitor, registered and activated.
        fn active_app(&self) -> App {
            let mut app = self.app(vec![apple_spec()]);
            assert_eq!(app.register(&self.adapter), Reply::Accepted);
            assert_eq!(app.next(), heard("Activate", MONITOR_PATH, None));
            self.bluez.wait_monitors(1, HANG_LIMIT);
            app
        }

        fn finish(self) {
            self.bluez.finish();
        }
    }

    /// Makes one call on the adapter, answered with success, and finds it the whole record.
    fn records<B>(interface: &str, method: &str, body: &B, recorded: DeviceMethod)
    where
        B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
    {
        let bench = Bench::new();
        bench
            .app(Vec::new())
            .call(&bench.adapter.0, interface, method, body);
        let object = bench.adapter.0.clone();
        let call = Call {
            object,
            method: recorded,
            argument: None,
        };
        assert_eq!(bench.bluez.calls(), [call]);
        bench.finish();
    }

    #[test]
    fn a_registered_monitor_is_listed_with_its_pattern() {
        let bench = Bench::new();
        let mut app = bench.app(vec![apple_spec()]);
        assert_eq!(app.register(&bench.adapter), Reply::Accepted);
        assert_eq!(app.next(), heard("Activate", MONITOR_PATH, None));
        assert_eq!(bench.bluez.wait_monitors(1, HANG_LIMIT), [apple_monitor()]);
        assert_eq!(app.reads(), 1, "BlueZ reads the application once");
        bench.finish();
    }

    #[test]
    fn a_matching_advert_reaches_device_found() {
        let bench = Bench::new();
        let mut app = bench.active_app();
        let _advertiser = bench.bluez.advertise(FIRST, &PROXIMITY);
        assert_eq!(app.next(), found(FIRST_ON_HCI0));
        assert_eq!(app.manufacturer_data(FIRST_ON_HCI0), PROXIMITY.to_vec());
        bench.finish();
    }

    #[test]
    fn a_foreign_advert_reaches_no_monitor() {
        let bench = Bench::new();
        let mut app = bench.active_app();
        let _foreign = bench.bluez.advertise(FIRST, &NEARBY);
        let _ours = bench.bluez.advertise(SECOND, &PROXIMITY);
        assert_eq!(
            app.next(),
            found(SECOND_ON_HCI0),
            "the first call names the matching advert, so the foreign one made none"
        );
        assert!(
            !app.has_object(FIRST_ON_HCI0),
            "BlueZ makes no device for an advert no monitor matches while it does not discover"
        );
        bench.finish();
    }

    #[test]
    fn an_update_changes_the_property() {
        let bench = Bench::new();
        let mut app = bench.active_app();
        let advertiser = bench.bluez.advertise(FIRST, &PROXIMITY);
        assert_eq!(app.next(), found(FIRST_ON_HCI0));
        let rule = format!(
            "type='signal',sender='{BLUEZ}',interface='{PROPERTIES}',member='PropertiesChanged',path='{FIRST_ON_HCI0}'"
        );
        let subscribe = MessageStream::for_match_rule(rule.as_str(), &app.client, None);
        let mut changes = in_time(&app.runtime, subscribe).unwrap();
        let mut next_advert = PROXIMITY;
        next_advert[BATTERY_BYTE] = NEXT_BATTERY;
        advertiser.update(&next_advert);
        let next = poll_fn(|context| Pin::new(&mut changes).poll_next(context));
        let signal = in_time(&app.runtime, next).unwrap().unwrap();
        let (interface, values, _): (String, HashMap<String, OwnedVariant>, Vec<String>) =
            signal.body().deserialize().unwrap();
        assert_eq!(interface, "org.bluez.Device1");
        assert!(values.contains_key("ManufacturerData"));
        assert_eq!(app.manufacturer_data(FIRST_ON_HCI0), next_advert.to_vec());
        assert_eq!(
            app.next(),
            found(FIRST_ON_HCI0),
            "a monitor with no RSSI parameters hears every matching advert"
        );
        bench.finish();
    }

    #[test]
    fn a_refused_registration_lists_nothing() {
        let bench = Bench::new();
        bench.bluez.refuse_monitors(true);
        let app = bench.app(vec![apple_spec()]);
        assert_eq!(app.register(&bench.adapter), refused(FAILED));
        assert_eq!(bench.bluez.monitors(), []);
        assert_eq!(app.reads(), 0, "a refused application is never read");
        bench.finish();
    }

    #[test]
    fn dropping_the_name_forgets_the_monitors() {
        let bench = Bench::new();
        let app = bench.active_app();
        assert_eq!(bench.bluez.monitors(), [apple_monitor()]);
        drop(app);
        bench.bluez.wait_monitors(0, HANG_LIMIT);
        bench.finish();
    }

    #[test]
    fn a_second_registration_of_the_same_application_already_exists() {
        let bench = Bench::new();
        let app = bench.active_app();
        assert_eq!(app.register(&bench.adapter), refused(ALREADY_EXISTS));
        assert_eq!(bench.bluez.monitors(), [apple_monitor()]);
        bench.finish();
    }

    #[test]
    fn an_unregistered_application_hears_release() {
        let bench = Bench::new();
        let mut app = bench.active_app();
        assert_eq!(app.unregister(&bench.adapter), Reply::Accepted);
        assert_eq!(app.next(), heard("Release", MONITOR_PATH, None));
        assert_eq!(bench.bluez.monitors(), []);
        assert_eq!(
            app.register(&bench.adapter),
            Reply::Accepted,
            "an unregistered application is gone, not only released"
        );
        bench.finish();
    }

    #[test]
    fn released_monitors_leave_the_list_and_the_application_stays_registered() {
        let bench = Bench::new();
        let mut app = bench.active_app();
        bench.bluez.release_monitors();
        assert_eq!(app.next(), heard("Release", MONITOR_PATH, None));
        assert_eq!(bench.bluez.monitors(), []);
        assert_eq!(app.register(&bench.adapter), refused(ALREADY_EXISTS));
        bench.finish();
    }

    #[test]
    fn an_invalid_monitor_hears_release_and_the_valid_one_activate() {
        let bench = Bench::new();
        let invalid = Spec {
            path: TRACKING_PATH,
            kind: "and_patterns",
            ..apple_spec()
        };
        let mut app = bench.app(vec![apple_spec(), invalid]);
        assert_eq!(app.register(&bench.adapter), Reply::Accepted);
        assert_eq!(app.next(), heard("Release", TRACKING_PATH, None));
        assert_eq!(app.next(), heard("Activate", MONITOR_PATH, None));
        assert_eq!(bench.bluez.wait_monitors(1, HANG_LIMIT), [apple_monitor()]);
        bench.finish();
    }

    #[test]
    fn a_tracking_monitor_hears_device_lost_and_a_plain_one_does_not() {
        let bench = Bench::new();
        let tracking = Spec {
            path: TRACKING_PATH,
            tracking: true,
            ..apple_spec()
        };
        let mut app = bench.app(vec![apple_spec(), tracking]);
        assert_eq!(app.register(&bench.adapter), Reply::Accepted);
        assert_eq!(app.next(), heard("Activate", MONITOR_PATH, None));
        assert_eq!(app.next(), heard("Activate", TRACKING_PATH, None));
        bench.bluez.wait_monitors(2, HANG_LIMIT);
        let first = bench.bluez.advertise(FIRST, &PROXIMITY);
        assert_eq!(app.next(), found(FIRST_ON_HCI0));
        assert_eq!(
            app.next(),
            heard("DeviceFound", TRACKING_PATH, Some(FIRST_ON_HCI0))
        );
        first.lose();
        let _second = bench.bluez.advertise(SECOND, &PROXIMITY);
        assert_eq!(
            app.next(),
            heard("DeviceLost", TRACKING_PATH, Some(FIRST_ON_HCI0))
        );
        assert_eq!(
            app.next(),
            found(SECOND_ON_HCI0),
            "the plain monitor's next call is the second advert, so it heard no loss"
        );
        bench.finish();
    }

    #[test]
    fn an_adapter_at_a_chosen_path_takes_a_registration() {
        let bench = Bench::new();
        let hci7 = bench.bluez.add_adapter_at("hci7");
        assert_eq!(hci7.0, "/org/bluez/hci7");
        bench.bluez.add_monitor_manager(&hci7);
        let mut app = bench.app(vec![apple_spec()]);
        assert_eq!(app.register(&hci7), Reply::Accepted);
        assert_eq!(app.next(), heard("Activate", MONITOR_PATH, None));
        bench.bluez.wait_monitors(1, HANG_LIMIT);
        let _advertiser = bench.bluez.advertise(FIRST, &PROXIMITY);
        assert_eq!(
            app.next(),
            heard(
                "DeviceFound",
                MONITOR_PATH,
                Some("/org/bluez/hci7/dev_02_00_00_00_00_01")
            )
        );
        bench.finish();
    }

    #[test]
    fn a_registration_without_the_manager_fails_as_a_missing_interface_does() {
        let bench = Bench::new();
        bench.bluez.remove_monitor_manager(&bench.adapter);
        let app = bench.app(vec![apple_spec()]);
        assert_eq!(app.register(&bench.adapter), refused(UNKNOWN_METHOD));
        assert_eq!(app.reads(), 0);
        bench.finish();
    }

    #[test]
    fn a_powered_off_adapter_hears_no_advert() {
        let bench = Bench::new();
        let mut app = bench.active_app();
        bench.bluez.power(&bench.adapter, false);
        let _unheard = bench.bluez.advertise(FIRST, &PROXIMITY);
        bench.bluez.power(&bench.adapter, true);
        let _heard = bench.bluez.advertise(SECOND, &PROXIMITY);
        assert_eq!(app.next(), found(SECOND_ON_HCI0));
        bench.finish();
    }

    #[test]
    fn start_discovery_is_recorded() {
        records(ADAPTER, "StartDiscovery", &(), DeviceMethod::StartDiscovery);
    }

    #[test]
    fn stop_discovery_is_recorded() {
        records(ADAPTER, "StopDiscovery", &(), DeviceMethod::StopDiscovery);
    }

    #[test]
    fn set_discovery_filter_is_recorded() {
        let filter = (HashMap::from([("Transport", Variant::from("le"))]),);
        records(
            ADAPTER,
            "SetDiscoveryFilter",
            &filter,
            DeviceMethod::SetDiscoveryFilter,
        );
    }

    #[test]
    fn a_powered_write_is_recorded_with_its_value() {
        let property = "Powered".to_owned();
        let set = DeviceMethod::Set {
            property,
            value: "false".to_owned(),
        };
        records(
            PROPERTIES,
            "Set",
            &(ADAPTER, "Powered", Variant::from(false)),
            set,
        );
    }
}
