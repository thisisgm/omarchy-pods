//! The installer `packaging/setup` run as written, inside a temporary directory whose `PATH` holds only stubs and an allow list of real programs.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::disallowed_methods,
    reason = "test helpers outside a test function fail the test by panicking, and poll real time for the run they watch"
)]

mod bench;
mod install;
mod monitor;
mod old_install;
