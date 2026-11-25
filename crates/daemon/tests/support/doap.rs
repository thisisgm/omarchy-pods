//! The fake pods' dictation service behind the trap: the route of a device's attribute channel.

use std::sync::Arc;
use std::time::Duration;

use pods_proto::packets::ATT_PSM;
use pods_trap::{Answer, Route};

use super::rig::Device;

/// The route that hands each dial of the device's attribute channel to its fake pods.
pub(crate) fn route(device: &Device) -> Route {
    let pods = device.pods.clone();
    Route {
        device: device.address,
        psm: ATT_PSM,
        answer: Answer::Accept {
            after: Duration::ZERO,
            serve: Arc::new(move |channel| pods.serve_att(channel)),
        },
    }
}
