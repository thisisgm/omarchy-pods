//! Single-property reads and the scripted pairing error.

use zbus::Message;

use super::{Answer, Bluez, DEVICE_INTERFACE, Objects, UNKNOWN};

impl Bluez {
    /// Answers a `Device1` `Paired` read with an `UnknownObject` reply carrying a fixture path.
    pub(crate) fn fail_paired_read(&self) {
        self.lock().fail_paired_read = true;
    }
}

impl Objects {
    pub(super) fn read_property(&self, call: &Message, path: &str) -> Answer {
        let header = call.header();
        // Sample input: ("org.bluez.Device1", "Paired").
        let body = call.body();
        let requested = body.deserialize::<(&str, &str)>();
        if self.fail_paired_read && requested == Ok((DEVICE_INTERFACE, "Paired")) {
            let reply = Message::error(&header, "org.freedesktop.DBus.Error.UnknownObject")
                .build(&("No such object /org/bluez/hci0/dev_00_11_22_33_44_55",))
                .unwrap();
            return Answer::Send(vec![reply]);
        }
        let property = requested
            .ok()
            .and_then(|(_, name)| self.properties(path)?.remove(name));
        let reply = match property {
            Some(value) => Message::method_return(&header).build(&value),
            None => Message::error(&header, UNKNOWN).build(&("No such property",)),
        };
        Answer::Send(vec![reply.unwrap()])
    }
}
