//! Parsers and encoders for the headphone control channel, with no I/O.
#![forbid(unsafe_code)]

pub mod addr;
pub mod att;
pub mod battery;
pub mod control;
pub mod dictation;
pub mod ear;
pub mod error;
#[cfg(test)]
mod fixture;
pub mod model;
pub mod noise;
pub mod packets;
pub mod sdulog;
pub mod uplink;
