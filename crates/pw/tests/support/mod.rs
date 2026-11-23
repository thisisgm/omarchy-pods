//! Shared by the test files that load the client library into their own process.

use std::path::Path;

use pods_pw::Spec;

pub(crate) const NAME: &str = "pods-source";
pub(crate) const DESCRIPTION: &str = "Pods microphone";
pub(crate) const RATE: u32 = 64_000;
pub(crate) const LATENCY_NS: u64 = 85_000_000;
pub(crate) const PRIORITY: u32 = 3100;

/// The node every test opens, on the server at `remote`.
pub(crate) fn spec(remote: &Path) -> Spec<'_> {
    Spec {
        remote,
        name: NAME,
        description: DESCRIPTION,
        rate: RATE,
        latency_ns: LATENCY_NS,
        priority: PRIORITY,
    }
}

/// Gives this process the directories and the dead bus addresses the rig gives its children, so the library reads no user configuration and asks no message bus for real time priority.
#[expect(
    unsafe_code,
    clippy::disallowed_methods,
    reason = "the library takes its configuration and its buses from the environment"
)]
pub(crate) fn isolate(dir: &Path) {
    let alone = std::env::var_os("NEXTEST").is_some();
    assert!(alone, "needs cargo nextest: one process per test");
    // The rig also removes the variables that name a session server; the binding takes its server from `Spec` alone, which a test of its own proves.
    for (name, value) in pods_pwrig::environment(dir) {
        if let Some(value) = value {
            // SAFETY: nextest gives each test a process of its own, and this runs before the library starts a thread in it.
            unsafe { std::env::set_var(name, value) };
        }
    }
}
