//! The fake BlueZ against a plain bus client.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the fake BlueZ is read by a bus client here, so the daemon helpers the other suites share go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::future::poll_fn;
    use std::pin::Pin;

    use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
    use tokio::time::timeout;
    use zbus::connection::Builder;
    use zbus::export::futures_core::Stream;
    use zbus::{Connection, MessageStream, OwnedObjectPath, OwnedVariant};

    use super::support::bluez::{AAP_UUID, DEVICE_INTERFACE, FakeBluez};
    use super::support::bus::start_bus;
    use super::support::{HANG_LIMIT, Session};

    const FIRST_ADDRESS: &str = "02:00:00:00:00:01";
    const DEVICE_CHANGES: &str = "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',arg0='org.bluez.Device1'";

    type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedVariant>>>;

    async fn managed_objects(client: &Connection) -> Managed {
        client
            .call_method(
                Some("org.bluez"),
                "/",
                Some("org.freedesktop.DBus.ObjectManager"),
                "GetManagedObjects",
                &(),
            )
            .await
            .unwrap()
            .body()
            .deserialize()
            .unwrap()
    }

    fn in_time<T>(runtime: &Runtime, work: impl Future<Output = T>) -> T {
        let finished = runtime.block_on(async { timeout(HANG_LIMIT, work).await });
        finished.unwrap_or_else(|_| panic!("the bus client still waiting after {HANG_LIMIT:?}"))
    }

    #[test]
    fn the_fake_bluez_serves_its_objects_to_a_client() {
        let session = Session::new();
        let bus = start_bus(&session, "system");
        let bluez = FakeBluez::start(&bus.address, true);
        let adapter = bluez.add_adapter();
        let path = bluez.add_device(&adapter, FIRST_ADDRESS.parse().unwrap(), &[AAP_UUID], false);
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (client, mut changes) = in_time(&runtime, async {
            let client = Builder::address(bus.address.as_str())
                .build()
                .await
                .unwrap();
            let objects = managed_objects(&client).await;
            let adapter_path = OwnedObjectPath::try_from(adapter.0.as_str()).unwrap();
            assert!(objects[&adapter_path].contains_key("org.bluez.Adapter1"));
            let device_path = OwnedObjectPath::try_from(path.0.as_str()).unwrap();
            let device = &objects[&device_path][DEVICE_INTERFACE];
            let address = String::try_from(device["Address"].try_clone().unwrap()).unwrap();
            assert_eq!(address, FIRST_ADDRESS);
            assert!(!bool::try_from(&device["Connected"]).unwrap());
            let uuids = Vec::<String>::try_from(device["UUIDs"].try_clone().unwrap()).unwrap();
            assert_eq!(uuids, [AAP_UUID]);
            let changes = MessageStream::for_match_rule(DEVICE_CHANGES, &client, None)
                .await
                .unwrap();
            (client, changes)
        });
        bluez.set(&path, "Connected", OwnedVariant::from(true));
        let next = poll_fn(|context| Pin::new(&mut changes).poll_next(context));
        let message = in_time(&runtime, next).unwrap().unwrap();
        assert_eq!(message.header().path().unwrap().as_str(), path.0);
        let (interface, values, _): (String, HashMap<String, OwnedVariant>, Vec<String>) =
            message.body().deserialize().unwrap();
        assert_eq!(interface, DEVICE_INTERFACE);
        assert!(bool::try_from(&values["Connected"]).unwrap());
        let objects = in_time(&runtime, managed_objects(&client));
        let device_path = OwnedObjectPath::try_from(path.0.as_str()).unwrap();
        assert!(bool::try_from(&objects[&device_path][DEVICE_INTERFACE]["Connected"]).unwrap());
    }
}
