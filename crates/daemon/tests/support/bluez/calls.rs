//! The calls the fake BlueZ records: device and adapter methods, monitor registrations, and adapter property writes.

use zbus::names::{InterfaceName, MemberName};
use zbus::{Message, OwnedObjectPath, OwnedVariant};

use super::monitor::MANAGER_INTERFACE;
use super::{ADAPTER_INTERFACE, DEVICE_INTERFACE, PROPERTIES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeviceMethod {
    Connect,
    Disconnect,
    RemoveDevice,
    StartDiscovery,
    StopDiscovery,
    SetDiscoveryFilter,
    /// `AdvertisementMonitorManager1` calls, on an adapter that serves the interface or not.
    RegisterMonitor,
    UnregisterMonitor,
    /// A `Properties.Set` on `Adapter1`, the value as D-Bus text writes it: `false`.
    Set {
        property: String,
        value: String,
    },
}

/// A method call on a device or an adapter; `argument` is the device path a `RemoveDevice` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Call {
    pub(crate) object: String,
    pub(crate) method: DeviceMethod,
    pub(crate) argument: Option<String>,
}

/// The call as `calls` lists it: `Connect` or `Disconnect` on `Device1`, a method of `Adapter1` or the monitor manager that `DeviceMethod` names, or a `Set` on `Adapter1`.
pub(super) fn record(call: &Message) -> Option<Call> {
    let header = call.header();
    let interface = header.interface().map(InterfaceName::as_str);
    let member = header.member().map(MemberName::as_str);
    let (method, argument) = match (interface, member) {
        (Some(DEVICE_INTERFACE), Some("Connect")) => (DeviceMethod::Connect, None),
        (Some(DEVICE_INTERFACE), Some("Disconnect")) => (DeviceMethod::Disconnect, None),
        // Sample body: ("/org/bluez/hci0/dev_02_00_00_00_00_01").
        (Some(ADAPTER_INTERFACE), Some("RemoveDevice")) => {
            let device: OwnedObjectPath = call.body().deserialize().ok()?;
            (DeviceMethod::RemoveDevice, Some(device.as_str().to_owned()))
        }
        (Some(ADAPTER_INTERFACE), Some("StartDiscovery")) => (DeviceMethod::StartDiscovery, None),
        (Some(ADAPTER_INTERFACE), Some("StopDiscovery")) => (DeviceMethod::StopDiscovery, None),
        (Some(ADAPTER_INTERFACE), Some("SetDiscoveryFilter")) => {
            (DeviceMethod::SetDiscoveryFilter, None)
        }
        (Some(MANAGER_INTERFACE), Some("RegisterMonitor")) => (DeviceMethod::RegisterMonitor, None),
        (Some(MANAGER_INTERFACE), Some("UnregisterMonitor")) => {
            (DeviceMethod::UnregisterMonitor, None)
        }
        // Sample body: ("org.bluez.Adapter1", "Powered", <false>).
        (Some(PROPERTIES), Some("Set")) => {
            let (interface, property, value): (String, String, OwnedVariant) =
                call.body().deserialize().ok()?;
            if interface != ADAPTER_INTERFACE {
                return None;
            }
            let value = value.to_string();
            (DeviceMethod::Set { property, value }, None)
        }
        _ => return None,
    };
    Some(Call {
        object: header.path()?.as_str().to_owned(),
        method,
        argument,
    })
}
