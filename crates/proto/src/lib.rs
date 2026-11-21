//! Parsers and encoders for the headphone control channel, with no I/O.
#![forbid(unsafe_code)]

pub mod battery;
pub mod control;
#[cfg(test)]
mod cpp_header;
pub mod ear;
pub mod error;
pub mod noise;
pub mod packets;
