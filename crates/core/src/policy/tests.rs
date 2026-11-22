//! Support tests: the disconnect notice rule, and its text pinned to the C++ daemon's.

use cpp_source::MAIN;
use pods_proto::addr::BdAddr;

use super::notices::{Notice, disconnected};
use crate::link::Signal;

/// A synthetic address, never a real one.
const DEVICE: &str = "02:00:00:00:00:01";

fn device() -> BdAddr {
    DEVICE.parse().unwrap()
}

/// The text of each `tr("...")` in `call`; sample input `notify(\n tr("AirPods Disconnected"),\n tr("Your AirPods have been disconnected")`.
fn translated(call: &str) -> Vec<&str> {
    call.split("tr(\"")
        .skip(1)
        .map(|piece| piece.split_once("\")").unwrap().0)
        .collect()
}

#[test]
fn a_device_lost_after_its_link_was_up_shows_the_disconnect_notice() {
    let lost = Signal::Lost {
        device: device(),
        populated: true,
    };
    assert_eq!(disconnected(lost), Some(Notice::Disconnected));
}

#[test]
fn a_device_lost_before_its_link_came_up_shows_nothing() {
    let lost = Signal::Lost {
        device: device(),
        populated: false,
    };
    assert_eq!(disconnected(lost), None);
}

#[test]
fn the_link_coming_up_shows_nothing() {
    let up = Signal::Up { device: device() };
    assert_eq!(disconnected(up), None);
}

#[test]
fn the_disconnect_text_is_the_cpp_daemons() {
    let call = MAIN.between("if (!m_isSuspending) {", ");");
    let notice = Notice::Disconnected;
    assert_eq!(translated(call), [notice.title(), notice.message()]);
}
