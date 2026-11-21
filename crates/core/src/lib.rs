//! The daemon state, the commands that change it and the status document, with no I/O.
#![forbid(unsafe_code)]

pub mod command;
pub mod legacy;
pub mod state;
pub mod status;
