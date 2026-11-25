//! The fenced crate: the only one in the workspace that may hold an `unsafe` item.
#![cfg_attr(
    test,
    expect(
        dead_code_pub_in_binary,
        reason = "the trap integration tests exercise `connect_with_imtu` without Bluetooth hardware"
    )
)]

pub mod l2cap;
