//! A barrier for the daemon's BlueZ signals: a path that is no device and holds no address, whose `UUIDs` change the daemon reads back.

use std::collections::HashMap;

use zbus::message::Header;
use zbus::names::MemberName;
use zbus::{ObjectPath, OwnedVariant, Variant};

use super::super::wait_for;
use super::{Bluez, DEVICE_INTERFACE, PROPERTIES};

const PATH: &str = "/org/bluez/barrier";

/// How many times the daemon read the barrier path.
#[derive(Default)]
pub(super) struct Barrier {
    reads: usize,
}

impl Barrier {
    /// No properties, so the daemon keeps nothing for the path it read.
    pub(super) fn properties(path: &str) -> Option<HashMap<String, OwnedVariant>> {
        (path == PATH).then(HashMap::new)
    }

    pub(super) fn note(&mut self, call: &Header<'_>) {
        let member = call.member().map(MemberName::as_str);
        if member == Some("GetAll") && call.path().map(ObjectPath::as_str) == Some(PATH) {
            self.reads = self.reads.saturating_add(1);
        }
    }
}

impl Bluez {
    /// Returns once the daemon took every signal emitted before the call, as one ordered stream brings the barrier's change behind them.
    pub(crate) fn wait_signals_taken(&self) {
        let before = self.lock().barrier.reads;
        let no_uuids = OwnedVariant::try_from(Variant::from(Vec::<String>::new())).unwrap();
        let changed = HashMap::from([("UUIDs", no_uuids)]);
        let body = (DEVICE_INTERFACE, changed, Vec::<String>::new());
        self.emit(PATH, PROPERTIES, "PropertiesChanged", &body);
        wait_for("the barrier read behind the signals", || {
            (self.lock().barrier.reads > before).then_some(())
        });
    }
}
