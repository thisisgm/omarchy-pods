//! The built binary's `hw link` command against stub programs: every tool it runs is a script in a temporary directory.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::disallowed_methods,
    reason = "test helpers outside a test function fail the test by panicking, and wait on real time for the run they watch"
)]

mod attended;
mod bench;
mod burst;
mod checks;
mod guard;
mod old_line;
