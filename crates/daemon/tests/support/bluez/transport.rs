//! The fake BlueZ's media transports, each under the device it carries audio to.

use std::collections::HashMap;

use zbus::{OwnedVariant, Str, Variant};

use super::{Managed, object};

pub(crate) const TRANSPORT_INTERFACE: &str = "org.bluez.MediaTransport1";
/// BlueZ lists a transport under its local endpoint, and a computer sending music is the A2DP source.
pub(crate) const A2DP_SOURCE_UUID: &str = "0000110a-0000-1000-8000-00805f9b34fb";
/// No stream holds the transport; BlueZ moves it to `active` while audio flows.
const IDLE: &str = "idle";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransportPath(pub(crate) String);

struct Transport {
    device: String,
    codec: u8,
    configuration: Vec<u8>,
}

/// Every transport the fake holds, by object path.
#[derive(Default)]
pub(super) struct Transports {
    held: HashMap<String, Transport>,
}

impl Transports {
    /// The new transport's path: `<device>/sep1/fd<n>`, `n` counting the device's transports.
    pub(super) fn add(&mut self, device: &str, codec: u8, configuration: &[u8]) -> String {
        let count = self.of(device).count();
        let path = format!("{device}/sep1/fd{count}");
        let transport = Transport {
            device: device.to_owned(),
            codec,
            configuration: configuration.to_vec(),
        };
        self.held.insert(path.clone(), transport);
        path
    }

    /// The paths of the device's transports, which are gone once this returns.
    pub(super) fn remove_device(&mut self, device: &str) -> Vec<String> {
        let paths: Vec<String> = self.of(device).map(str::to_owned).collect();
        for path in &paths {
            self.held.remove(path);
        }
        paths
    }

    pub(super) fn properties(&self, path: &str) -> Option<HashMap<String, OwnedVariant>> {
        self.held.get(path).map(Transport::properties)
    }

    /// The interfaces `InterfacesAdded` and `GetManagedObjects` list at the transport's path.
    pub(super) fn interfaces(
        &self,
        path: &str,
    ) -> Option<HashMap<String, HashMap<String, OwnedVariant>>> {
        let properties = self.properties(path)?;
        Some(HashMap::from([(
            TRANSPORT_INTERFACE.to_owned(),
            properties,
        )]))
    }

    pub(super) fn managed(&self) -> Managed {
        let listed = self.held.keys().map(|path| {
            let interfaces = self.interfaces(path).unwrap();
            (object(path).into(), interfaces)
        });
        listed.collect()
    }

    fn of<'held>(&'held self, device: &'held str) -> impl Iterator<Item = &'held str> {
        let paths = self.held.iter();
        let owned = paths.filter(move |(_, transport)| transport.device == device);
        owned.map(|(path, _)| path.as_str())
    }
}

impl Transport {
    fn properties(&self) -> HashMap<String, OwnedVariant> {
        let configuration =
            OwnedVariant::try_from(Variant::from(self.configuration.clone())).unwrap();
        HashMap::from([
            (
                "Device".to_owned(),
                OwnedVariant::from(object(&self.device)),
            ),
            (
                "UUID".to_owned(),
                OwnedVariant::from(Str::from(A2DP_SOURCE_UUID)),
            ),
            ("Codec".to_owned(), OwnedVariant::from(self.codec)),
            ("Configuration".to_owned(), configuration),
            ("State".to_owned(), OwnedVariant::from(Str::from(IDLE))),
        ])
    }
}
