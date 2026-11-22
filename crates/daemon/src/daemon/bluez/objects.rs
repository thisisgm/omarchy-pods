//! The AirPods BlueZ knows, by object path, and the device events each change makes.

use std::collections::{BTreeMap, HashMap};

use pods_core::link::ladder::KNOWN_MAX;
use pods_proto::addr::BdAddr;
use pods_proto::model::{Product, product, product_of_modalias};
use zbus::{Array, OwnedVariant, Variant};

use super::AAP_UUID;
use crate::daemon::link::DeviceEvent;

/// A signal names only the properties that changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Properties {
    pub(super) address: Option<BdAddr>,
    /// `None` when `UUIDs` is not named.
    pub(super) kind: Option<Kind>,
    pub(super) connected: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Pods,
    /// Lists `AAP_UUID` under a Beats `Modalias`, so it is never kept as a candidate.
    Beats,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Change {
    Device {
        path: String,
        properties: Properties,
    },
    Removed {
        path: String,
    },
    /// From `GetManagedObjects`.
    Swept(Vec<(String, Properties)>),
    /// `org.bluez` has no owner, so none of its objects stands.
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    device: BdAddr,
    connected: bool,
}

/// One address may stand under several paths, one per adapter; at most `KNOWN_MAX` are kept.
#[derive(Debug, Default)]
pub(super) struct Candidates {
    paths: BTreeMap<String, Entry>,
}

impl Properties {
    /// Sample input: `{"Address": <"02:00:00:00:00:01">, "UUIDs": <["74ec2172-0bad-4d01-8f77-997b2be0722a"]>, "Modalias": <"bluetooth:v004Cp2027d0000">, "Connected": <true>}`; another type reads as absent.
    pub(super) fn read(properties: &HashMap<String, OwnedVariant>) -> Self {
        let value = |name: &str| properties.get(name).map(|value| &**value);
        let modalias = value("Modalias").and_then(|modalias| <&str>::try_from(modalias).ok());
        Self {
            address: value("Address")
                .and_then(|address| <&str>::try_from(address).ok())
                .and_then(|address| address.parse().ok()),
            kind: value("UUIDs")
                .and_then(lists_aap)
                .map(|lists_aap| kind(lists_aap, modalias)),
            connected: value("Connected").and_then(|connected| bool::try_from(connected).ok()),
        }
    }
}

/// A device that lists `AAP_UUID` is pods unless its `Modalias` names a Beats product; one that does not parse names none.
fn kind(lists_aap: bool, modalias: Option<&str>) -> Kind {
    if !lists_aap {
        return Kind::Other;
    }
    match modalias.and_then(product_of_modalias).map(product) {
        Some(Product::Beats) => Kind::Beats,
        Some(Product::AirPods(_) | Product::Unknown) | None => Kind::Pods,
    }
}

fn lists_aap(uuids: &Variant<'_>) -> Option<bool> {
    let uuids = <&Array<'_>>::try_from(uuids).ok()?;
    Some(
        uuids.iter().any(|uuid| {
            <&str>::try_from(uuid).is_ok_and(|uuid| uuid.eq_ignore_ascii_case(AAP_UUID))
        }),
    )
}

impl Change {
    pub(super) fn names_beats(&self) -> bool {
        let beats = |properties: &Properties| properties.kind == Some(Kind::Beats);
        match self {
            Self::Device { properties, .. } => beats(properties),
            Self::Swept(devices) => devices.iter().any(|(_, properties)| beats(properties)),
            Self::Removed { .. } | Self::Lost => false,
        }
    }
}

impl Candidates {
    pub(super) fn holds(&self, path: &str) -> bool {
        self.paths.contains_key(path)
    }

    /// A connected path first; `None` when BlueZ holds no object of `device`.
    pub(super) fn path_of(&self, device: BdAddr) -> Option<&str> {
        let mut paths = self
            .paths
            .iter()
            .filter(|(_, entry)| entry.device == device);
        let connected = paths.clone().find(|(_, entry)| entry.connected);
        connected
            .or_else(|| paths.next())
            .map(|(path, _)| path.as_str())
    }

    pub(super) fn apply(&mut self, change: Change) -> Vec<DeviceEvent> {
        match change {
            Change::Device { path, properties } => {
                self.update(path, properties).into_iter().collect()
            }
            Change::Removed { path } => self.remove(&path).into_iter().collect(),
            Change::Swept(devices) => self.replace(devices),
            Change::Lost => {
                self.paths.clear();
                vec![DeviceEvent::Lost]
            }
        }
    }

    /// A path not kept yet takes only what `properties` says.
    fn update(&mut self, path: String, properties: Properties) -> Option<DeviceEvent> {
        let kept = self.paths.get(&path).copied();
        let device = kept.map(|entry| entry.device).or(properties.address)?;
        let before = self.presence(device);
        if properties
            .kind
            .map_or(kept.is_some(), |kind| kind == Kind::Pods)
        {
            if kept.is_none() && self.paths.len() >= KNOWN_MAX {
                return None;
            }
            let connected = properties
                .connected
                .or(kept.map(|entry| entry.connected))
                .unwrap_or(false);
            self.paths.insert(path, Entry { device, connected });
        } else {
            self.paths.remove(&path);
        }
        edge(device, before, self.presence(device))
    }

    fn remove(&mut self, path: &str) -> Option<DeviceEvent> {
        let device = self.paths.get(path)?.device;
        let before = self.presence(device);
        self.paths.remove(path);
        edge(device, before, self.presence(device))
    }

    /// Reports every dropped address gone and every kept one again.
    fn replace(&mut self, devices: Vec<(String, Properties)>) -> Vec<DeviceEvent> {
        let before = self.devices();
        self.paths.clear();
        for (path, properties) in devices {
            if let (Some(device), Some(Kind::Pods)) = (properties.address, properties.kind)
                && self.paths.len() < KNOWN_MAX
            {
                let connected = properties.connected.unwrap_or(false);
                self.paths.insert(path, Entry { device, connected });
            }
        }
        let kept = self.devices();
        let gone = before
            .into_iter()
            .filter(|device| !kept.contains(device))
            .map(DeviceEvent::Gone);
        let reported = kept
            .iter()
            .filter_map(|device| edge(*device, None, self.presence(*device)));
        gone.chain(reported).collect()
    }

    fn devices(&self) -> Vec<BdAddr> {
        let mut devices: Vec<BdAddr> = Vec::new();
        for entry in self.paths.values() {
            if !devices.contains(&entry.device) {
                devices.push(entry.device);
            }
        }
        devices
    }

    /// `None` when no path of `device` is kept, else whether any of them is connected.
    fn presence(&self, device: BdAddr) -> Option<bool> {
        let mut paths = self
            .paths
            .values()
            .filter(|entry| entry.device == device)
            .peekable();
        paths.peek()?;
        Some(paths.any(|entry| entry.connected))
    }
}

fn edge(device: BdAddr, before: Option<bool>, after: Option<bool>) -> Option<DeviceEvent> {
    match (before, after) {
        (None | Some(false), Some(true)) => Some(DeviceEvent::Up(device)),
        (None | Some(true), Some(false)) => Some(DeviceEvent::Down(device)),
        (Some(_), None) => Some(DeviceEvent::Gone(device)),
        (None, None) | (Some(true), Some(true)) | (Some(false), Some(false)) => None,
    }
}

#[cfg(test)]
mod tests;
