//! The daemon state, the commands that change it and the status document, with no I/O.
#![forbid(unsafe_code)]

pub mod command;
pub mod legacy;
pub mod link;
pub mod policy;
pub mod prefs;
pub mod settings;
pub mod state;
pub mod status;
