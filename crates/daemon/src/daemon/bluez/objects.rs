//! The AirPods BlueZ knows, by object path, and the device events each change of them makes.

use std::collections::{BTreeMap, HashMap};

use pods_core::link::ladder::KNOWN_MAX;
use pods_proto::addr::BdAddr;
use zbus::{Array, OwnedVariant, Variant};

use super::AAP_UUID;
use crate::daemon::link::DeviceEvent;

/// What one source says of a device object; a signal names only the properties that changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Properties {
    pub(super) address: Option<BdAddr>,
    /// Whether `UUIDs` holds `AAP_UUID`.
    pub(super) lists_aap: Option<bool>,
    pub(super) connected: Option<bool>,
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
    /// Every device object BlueZ holds, from `GetManagedObjects`.
    Swept(Vec<(String, Properties)>),
    /// `org.bluez` has no owner, so none of its objects stands.
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    device: BdAddr,
    connected: bool,
}

/// One address may stand under several paths, one per adapter that knows it; at most `KNOWN_MAX` paths are kept.
#[derive(Debug, Default)]
pub(super) struct Candidates {
    paths: BTreeMap<String, Entry>,
}

impl Properties {
    /// Reads `Address`, `UUIDs` and `Connected` of `{"Address": <"02:00:00:00:00:01">, "UUIDs": <["74ec2172-0bad-4d01-8f77-997b2be0722a"]>, "Connected": <true>}`; a value of another type reads as absent.
    pub(super) fn read(properties: &HashMap<String, OwnedVariant>) -> Self {
        let value = |name: &str| properties.get(name).map(|value| &**value);
        Self {
            address: value("Address")
                .and_then(|address| <&str>::try_from(address).ok())
                .and_then(|address| address.parse().ok()),
            lists_aap: value("UUIDs").and_then(lists_aap),
            connected: value("Connected").and_then(|connected| bool::try_from(connected).ok()),
        }
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

impl Candidates {
    pub(super) fn holds(&self, path: &str) -> bool {
        self.paths.contains_key(path)
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

    /// Classifies the device at `path` again, a path not kept yet taking only what `properties` says.
    fn update(&mut self, path: String, properties: Properties) -> Option<DeviceEvent> {
        let kept = self.paths.get(&path).copied();
        let device = kept.map(|entry| entry.device).or(properties.address)?;
        let before = self.presence(device);
        if properties.lists_aap.unwrap_or(kept.is_some()) {
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

    /// Keeps only what the sweep found, reports every address it dropped gone and every address it kept.
    fn replace(&mut self, devices: Vec<(String, Properties)>) -> Vec<DeviceEvent> {
        let before = self.devices();
        self.paths.clear();
        for (path, properties) in devices {
            if let (Some(device), Some(true)) = (properties.address, properties.lists_aap)
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

    /// Every address kept, once each.
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

/// The event that moves the driver from what it was told of `device` to what holds now.
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
