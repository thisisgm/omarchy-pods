//! The link checks C1 to C4: adoption, stop, the status contract and reconnecting.

use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::time::{Duration, Instant};

use pods_core::link::STATE_POPULATED_LINE;

use super::lab::{
    ADOPTION_WAIT, DISCONNECT_WAIT, Lab, RECONNECT_WAIT, STATUS_TIMEOUT, millis, status_object,
    wait_until,
};
use super::report::{Bound, Check, Gate};
use super::system::{
    Entry, UNIT_STARTED_ID, Waited, between, find, journal_cursor, opening_line, phase_lines,
    wait_for_entries,
};
use super::unit::{self, UNDER_TEST, run, systemctl_ok};
use super::{Interrupted, stopped};

/// How long the unit may take from the stop call until it reports inactive.
const STOP_BOUND: Duration = Duration::from_secs(1);
/// Restarts in C1.
const ADOPTION_TRIALS: usize = 10;
/// Disconnect and connect rounds in C4.
const RECONNECT_TRIALS: usize = 5;
/// G1a: daemon start to populated.
const G1A: Bound = Bound {
    at_most_ms: 2000,
    needed: 10,
    of: 10,
};
/// G1b: `BlueZ` connected to populated.
const G1B: Bound = Bound {
    at_most_ms: 500,
    needed: 4,
    of: 5,
};
/// G4b: `do connect` reply to populated.
const G4B: Bound = Bound {
    at_most_ms: 15000,
    needed: 5,
    of: 5,
};

/// C1: ten restarts of the unit, each reaching a populated state; G1a is start to populated.
pub(crate) fn adoption(_lab: &Lab<'_>) -> Result<Check, Interrupted> {
    let mut check = Check::new("C1 adoption");
    let mut samples = Vec::new();
    let mut populated = 0_usize;
    for _ in 0..ADOPTION_TRIALS {
        if stopped() {
            return Err(Interrupted);
        }
        let cursor = match journal_cursor() {
            Ok(cursor) => cursor,
            Err(error) => {
                check.fail(error);
                continue;
            }
        };
        if let Err(error) = systemctl_ok(&["restart", UNDER_TEST]) {
            check.fail(error);
            continue;
        }
        let started_then_populated = |entries: &[Entry]| {
            let start = entries
                .iter()
                .position(|entry| entry.message_id.as_deref() == Some(UNIT_STARTED_ID))?;
            let done = find(entries, STATE_POPULATED_LINE, start)?;
            between(entries, start, done)
        };
        match wait_for_entries(cursor.as_deref(), ADOPTION_WAIT, |entries| {
            started_then_populated(entries).is_some()
        })? {
            Waited::Found(entries) => {
                populated = populated.saturating_add(1);
                samples.extend(started_then_populated(&entries));
            }
            Waited::TimedOut => {}
            Waited::ReadFailed(error) => check.fail(error),
        }
    }
    check.note(format!("populated {populated} of {ADOPTION_TRIALS}"));
    if populated != ADOPTION_TRIALS {
        check.fail("a restart did not reach a populated state");
    }
    check.gates.push(Gate {
        name: "G1a",
        samples_ms: samples,
        bound: Some(G1A),
    });
    Ok(check)
}

/// C2: a stop with the link up leaves no socket and no state file, and `BlueZ` keeps the device connected.
pub(crate) fn stop(lab: &Lab<'_>) -> Result<Check, Interrupted> {
    let mut check = Check::new("C2 stop");
    let connected_before = lab.device_connected();
    let began = Instant::now();
    let stopped_ok = systemctl_ok(&["stop", UNDER_TEST]);
    let activity = unit::activity(UNDER_TEST);
    let elapsed = began.elapsed();
    check.note(format!("stop took {} ms", millis(elapsed)));
    if let Err(error) = stopped_ok {
        check.fail(error);
    }
    match activity {
        Err(error) => check.fail(error),
        Ok(word) if unit::is_running(&word) || elapsed > STOP_BOUND => check.fail(format!(
            "the unit was not inactive within {} ms",
            millis(STOP_BOUND)
        )),
        Ok(_) => {}
    }
    for (name, path) in [("socket", lab.socket()), ("state file", lab.state_file())] {
        let gone = matches!(fs::symlink_metadata(&path), Err(error) if error.kind() == ErrorKind::NotFound);
        check.note(format!("{name} gone: {gone}"));
        if !gone {
            check.fail(format!("the {name} is still there"));
        }
    }
    match (connected_before, lab.device_connected()) {
        (Ok(false), _) => check.fail("BlueZ did not report the device connected before the stop"),
        (Err(error), _) | (_, Err(error)) => check.fail(error),
        (Ok(true), Ok(connected)) => {
            check.note(format!("BlueZ device connected: {connected}"));
            if !connected {
                check.fail("BlueZ dropped the device when the daemon stopped");
            }
        }
    }
    if let Err(error) = lab.start_again()? {
        check.fail(format!(
            "the unit did not start again to a populated state: {error}"
        ));
    }
    Ok(check)
}

/// C3: `librepods status` carries exactly the status document's keys, and the installed `librepods-ctl` prints the same line.
pub(crate) fn same_contract(lab: &Lab<'_>) -> Check {
    let mut check = Check::new("C3 same contract");
    let expected = document_keys();
    let line = |name: &str, ran: Result<unit::Ran, String>| match ran {
        Ok(ran) if ran.succeeded() => Ok(ran.stdout.trim_end().to_owned()),
        Ok(ran) => Err(format!("{name} status exited with {}", ran.status)),
        Err(error) => Err(error),
    };
    let before = line("librepods", lab.librepods(&["status"], STATUS_TIMEOUT));
    let run_dir = lab.run_dir();
    let ctl = line(
        "librepods-ctl",
        run(
            &unit::installed_ctl(lab.home),
            &[OsStr::new("status")],
            &[("XDG_RUNTIME_DIR", run_dir.as_os_str())],
            STATUS_TIMEOUT,
        ),
    );
    let after = line("librepods", lab.librepods(&["status"], STATUS_TIMEOUT));
    match before.as_deref().map(status_object) {
        Ok(Some(object)) => {
            let keys: Vec<&str> = object.keys().map(String::as_str).collect();
            check.note(format!(
                "{} keys, document has {}",
                keys.len(),
                expected.len()
            ));
            if keys != expected.iter().map(String::as_str).collect::<Vec<_>>() {
                check.fail("the keys of librepods status differ from the status document");
            }
        }
        Ok(None) => check.fail("librepods status printed no state object"),
        Err(error) => check.fail(error.as_str()),
    }
    // The state can change between two reads, so the ctl line must equal the line read just before or just after it.
    match ctl {
        Ok(ctl) if before.as_ref() == Ok(&ctl) || after.as_ref() == Ok(&ctl) => {
            check.note("librepods-ctl prints the same line");
        }
        Ok(_) => check.fail("the installed librepods-ctl printed a different line"),
        Err(error) => check.fail(error),
    }
    check
}

/// From the code that writes the status document, in its byte order.
pub(crate) fn document_keys() -> Vec<String> {
    let document = pods_core::status::to_status_object(&pods_core::state::State::default());
    document
        .as_object()
        .map_or_default(|object| object.keys().cloned().collect())
}

/// C4: five rounds of `do disconnect` then `do connect`; G1b and G4b come from each round.
pub(crate) fn reconnect(lab: &Lab<'_>) -> Result<Check, Interrupted> {
    let mut check = Check::new("C4 reconnect");
    let mut g1b = Vec::new();
    let mut g4b = Vec::new();
    let opening = opening_line();
    for round in 1..=RECONNECT_TRIALS {
        if let Err(error) = lab.act("disconnect") {
            check.fail(format!("round {round}: {error}"));
        } else if !wait_until(DISCONNECT_WAIT, || lab.connected() == Some(false))? {
            check.fail(format!(
                "round {round}: still connected {} s after do disconnect",
                DISCONNECT_WAIT.as_secs()
            ));
        }
        // A cursor that could not be read would let a populated line of an earlier round pass this one.
        let cursor = match journal_cursor() {
            Ok(cursor) => cursor,
            Err(error) => {
                check.fail(format!("round {round}: {error}"));
                continue;
            }
        };
        if let Err(error) = lab.act("connect") {
            check.fail(format!("round {round}: {error}"));
            continue;
        }
        let replied = Instant::now();
        let entries = match wait_for_entries(cursor.as_deref(), RECONNECT_WAIT, |entries| {
            find(entries, STATE_POPULATED_LINE, 0).is_some()
        })? {
            Waited::Found(entries) => entries,
            Waited::TimedOut => {
                check.fail(format!(
                    "round {round}: not populated {} s after do connect",
                    RECONNECT_WAIT.as_secs()
                ));
                continue;
            }
            Waited::ReadFailed(error) => {
                check.fail(format!("round {round}: {error}"));
                continue;
            }
        };
        g4b.push(millis(replied.elapsed()));
        let done = find(&entries, STATE_POPULATED_LINE, 0);
        let opened = entries
            .iter()
            .take(done.unwrap_or(0))
            .rposition(|entry| entry.message.contains(&opening));
        match (opened, done) {
            (Some(opened), Some(done)) => {
                let Some(sample) = between(&entries, opened, done) else {
                    check.fail(format!(
                        "round {round}: the populated line is stamped before the opening line"
                    ));
                    continue;
                };
                if sample > G1B.at_most_ms {
                    check.note(format!("round {round}: G1b {sample} ms, phases:"));
                    for phase in phase_lines(&entries, opened, done) {
                        check.note(format!("  {phase}"));
                    }
                }
                g1b.push(sample);
            }
            _ => check.fail(format!("round {round}: no {opening} line before populated")),
        }
    }
    check.gates.push(Gate {
        name: "G1b",
        samples_ms: g1b,
        bound: Some(G1B),
    });
    check.gates.push(Gate {
        name: "G4b",
        samples_ms: g4b,
        bound: Some(G4B),
    });
    Ok(check)
}

#[cfg(test)]
mod tests {
    use pods_core::status::{ADDITIVE_KEYS, CPP_KEY_COUNT};

    use super::document_keys;

    #[test]
    fn the_status_document_has_the_cpp_keys_and_the_additive_ones() {
        assert_eq!(document_keys().len(), CPP_KEY_COUNT + ADDITIVE_KEYS.len());
    }
}
