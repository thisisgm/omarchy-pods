//! The link through the built daemon: the fake BlueZ reports the pods, the trap answers the dial, the fake pods serve it, and the tests read what a user reads.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod link {
    mod devices;
    mod ladders;
    mod reports;
    mod sets;
    mod stop;
}
