//! Parsers and encoders for the packets the pods and a host exchange, with no I/O.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    expect(
        dead_code_pub_in_binary,
        reason = "the daemon and xtask consume the proximity advertising identifiers"
    )
)]

pub mod addr;
pub mod att;
pub mod awareness;
pub mod battery;
pub mod control;
pub mod dictation;
pub mod doap;
pub mod ear;
pub mod error;
#[cfg(test)]
mod fixture;
pub mod handshake;
pub mod keys;
pub mod metadata;
pub mod model;
pub mod noise;
pub mod packet;
pub mod packets;
pub mod proximity;
pub mod sdulog;
pub mod uplink;
