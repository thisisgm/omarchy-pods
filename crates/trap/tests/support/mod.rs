//! The stand-in's path and the guard that ends it, shared by both test binaries.

use std::process::Child;

pub(crate) const STAND_IN: &str = env!("CARGO_BIN_EXE_trap-dial");

/// A started stand-in, killed and reaped on every path out of the test.
pub(crate) struct Reaped(pub(crate) Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        // An error here means the child is already gone, which is what this guard wants.
        let _killed = self.0.kill();
        let _reaped = self.0.wait();
    }
}
