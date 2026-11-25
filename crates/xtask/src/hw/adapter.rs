//! C7, the adapter cycle: off, on, then `do connect`, the adapter powered back on on every way out.

use std::ffi::OsStr;
use std::time::{Duration, Instant};

use pods_core::link::STATE_POPULATED_LINE;

use super::Interrupted;
use super::lab::{DISCONNECT_WAIT, Lab, millis, settle, wait_until};
use super::report::{Check, Gate};
use super::system::{Waited, find, journal_cursor, wait_for_entries};
use super::unit::run;

/// How long the adapter has to show the power state asked of it.
const POWER_WAIT: Duration = Duration::from_secs(5);
/// How long `bluetoothctl power` may take.
const POWER_TIMEOUT: Duration = Duration::from_secs(15);

/// C7: the adapter off, on, then `do connect`; G4c is power on to populated.
pub(crate) fn check(lab: &Lab<'_>) -> Result<Check, Interrupted> {
    let mut check = Check::new("C7 adapter");
    // Without a link up first, `connected` going false after power off proves nothing.
    if lab.connected() != Some(true) {
        check.fail("not connected before the adapter went off");
    }
    let powered_off = power(lab, "off");
    let result = adapter_cycle(lab, &mut check, powered_off);
    // The adapter is powered back on whatever happened above, a stop signal included.
    if !lab.adapter_state().is_ok_and(|(powered, _)| powered) {
        let back = power(lab, "on");
        check.note(format!("adapter powered back on: {}", back.is_ok()));
        if let Err(error) = back {
            // A stop signal drops this check, so an adapter left off is reported now.
            crate::report(&format!("C7: {error}"));
            check.fail(error);
        }
    }
    result.map(|()| check)
}

fn adapter_cycle(
    lab: &Lab<'_>,
    check: &mut Check,
    powered_off: Result<(), String>,
) -> Result<(), Interrupted> {
    if let Err(error) = powered_off {
        check.fail(error);
        return Ok(());
    }
    let dropped = wait_until(DISCONNECT_WAIT, || lab.connected() == Some(false))?;
    check.note(format!("connected false after power off: {dropped}"));
    if !dropped {
        check.fail("still connected after the adapter went off");
    }
    let cursor = match journal_cursor() {
        Ok(cursor) => cursor,
        Err(error) => {
            check.fail(error);
            return Ok(());
        }
    };
    if let Err(error) = power(lab, "on") {
        check.fail(error);
        return Ok(());
    }
    let powered_on = Instant::now();
    if let Err(error) = lab.act("connect") {
        check.fail(error);
    }
    match wait_for_entries(cursor.as_deref(), lab.waits.connect, |entries| {
        find(entries, STATE_POPULATED_LINE, 0).is_some()
    })? {
        Waited::Found(_) => check.gates.push(Gate {
            name: "G4c",
            samples_ms: vec![millis(powered_on.elapsed())],
            bound: None,
        }),
        Waited::TimedOut => check.fail(format!(
            "not populated {} s after power on",
            lab.waits.connect.as_secs()
        )),
        Waited::ReadFailed(error) => check.fail(error),
    }
    Ok(())
}

/// Sets the adapter's power with `bluetoothctl power`, then requires `BlueZ` to show it.
fn power(lab: &Lab<'_>, state: &str) -> Result<(), String> {
    let args = [OsStr::new("power"), OsStr::new(state)];
    let ran = run(
        "bluetoothctl",
        &args,
        &[("LC_ALL", OsStr::new("C"))],
        POWER_TIMEOUT,
    )?;
    if !ran.succeeded() {
        return Err(format!(
            "bluetoothctl power {state} exited with {}",
            ran.status
        ));
    }
    // bluetoothctl can exit 0 after a refused change, so BlueZ's own property decides.
    let wanted = state == "on";
    let shown = settle(POWER_WAIT, || {
        lab.adapter_state()
            .is_ok_and(|(powered, _)| powered == wanted)
    });
    if shown {
        Ok(())
    } else {
        Err(format!("the adapter did not show power {state}"))
    }
}
