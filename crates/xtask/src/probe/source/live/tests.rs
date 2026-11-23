//! The unwind of a live run, in a copy of this test binary that panics with a stub `pactl` alone on its `PATH`, and the sink list's parser.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use pods_proto::uplink::STOP;
use pods_samples::uplink::state_marker;
use pods_sys::l2cap::{self, Channel, Received};
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

use super::{GUARD_SAYS, Running, device_sink};

/// Set only in the copy of this test binary that panics while the uplink runs.
const PANIC_CHILD: &str = "XTASK_PROBE_LIVE_PANIC_CHILD";
const EXECUTABLE: u32 = 0o755;
/// The far end's wait for one packet; the child's whole run ends well inside a few of them.
const FAR_POLL: Duration = Duration::from_secs(5);
/// A `pactl` that logs each call and lists the device's sink; builtins only, as `PATH` holds nothing else.
const PACTL: &str = r#"#!/bin/sh
printf 'pactl %s\n' "$*" >> "$STUB/calls"
case "$1" in
list) printf '57\tbluez_output.02_00_00_00_00_01.1\tPipeWire\ts16le 2ch 48000Hz\tSUSPENDED\n' ;;
esac
"#;
const LISTED: &str = "56\talsa_output.speakers\tPipeWire\ts16le 2ch 48000Hz\tRUNNING\n57\tbluez_output.02_00_00_00_00_01.1\tPipeWire\ts16le 2ch 48000Hz\tSUSPENDED\n";

#[test]
fn a_panic_while_the_uplink_runs_sends_stop_then_restores() {
    let stub = tempfile::tempdir().unwrap();
    let bin = stub.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("pactl"), PACTL).unwrap();
    fs::set_permissions(bin.join("pactl"), fs::Permissions::from_mode(EXECUTABLE)).unwrap();
    let child = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "--nocapture",
            "probe::source::live::tests::panic_while_the_uplink_runs",
        ])
        .env(PANIC_CHILD, "1")
        .env("PATH", &bin)
        .env("STUB", stub.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&child.stderr);
    assert!(!child.status.success(), "the child did not panic: {stderr}");
    assert!(stderr.contains("while the uplink runs"), "{stderr}");
    assert!(!stderr.contains(GUARD_SAYS), "{stderr}");
    let calls = fs::read_to_string(stub.path().join("calls")).unwrap();
    assert_eq!(
        calls.lines().collect::<Vec<_>>(),
        [
            "far end heard STOP",
            "pactl list short sinks",
            "pactl suspend-sink bluez_output.02_00_00_00_00_01.1 1",
            "pactl suspend-sink bluez_output.02_00_00_00_00_01.1 0",
        ]
    );
}

/// Runs only as the child of the test above, which reads what the uplink's guard did as the panic unwound.
#[test]
fn panic_while_the_uplink_runs() {
    let Some(stub) = env::var_os("STUB").filter(|_| env::var_os(PANIC_CHILD).is_some()) else {
        return;
    };
    let (near, far) = pair();
    near.set_recv_timeout(FAR_POLL).unwrap();
    thread::spawn(move || {
        far.set_recv_timeout(FAR_POLL).unwrap();
        let mut buf = [0_u8; 64];
        while let Ok(Received::Sdu(len)) = far.recv(&mut buf) {
            if buf.get(..len) == Some(&STOP[..]) {
                let calls = Path::new(&stub).join("calls");
                let mut log = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(calls)
                    .unwrap();
                log.write_all(b"far end heard STOP\n").unwrap();
                far.send(&state_marker(false)).unwrap();
            }
        }
    });
    let _running = Running {
        channel: &near,
        peer: "02:00:00:00:00:01".parse().unwrap(),
        pending: true,
    };
    panic!("a panic while the uplink runs");
}

#[test]
fn the_device_sink_is_the_first_listed_name_with_its_prefix() {
    let prefix = "bluez_output.02_00_00_00_00_01";
    assert_eq!(
        device_sink(LISTED, prefix),
        Some("bluez_output.02_00_00_00_00_01.1")
    );
    assert_eq!(device_sink(LISTED, "bluez_output.02_00_00_00_00_02"), None);
    assert_eq!(device_sink("", prefix), None);
    assert_eq!(
        device_sink("57 bluez_output.02_00_00_00_00_01.1\n", prefix),
        None
    );
}

/// Both ends of a channel; what one end sends waits in the other's queue.
#[expect(
    clippy::disallowed_methods,
    reason = "a test wraps both ends of a socket pair as channels"
)]
fn pair() -> (Channel, Channel) {
    let (near, far) = socketpair(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    (l2cap::from_fd(near), l2cap::from_fd(far))
}
