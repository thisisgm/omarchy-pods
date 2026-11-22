//! C9: the installed daemon's status line, read before the run stops it, against the line of the daemon under test on the same pods.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use pods_core::status::ADDITIVE_KEYS;
use rustix::fs::{Mode, OFlags};
use serde_json::Value;
use serde_json::value::RawValue;

use super::Interrupted;
use super::lab::{Lab, STATUS_TIMEOUT, status_object, wait_until};
use super::report::Check;
use super::unit::{installed_ctl, run};

const NAME: &str = "C9 old line";

/// Where each line is kept in the run's private directory, never in the report.
const OLD_FILE: &str = "old-line.json";
const NEW_FILE: &str = "new-line.json";

/// How long the daemon under test has to show the pods' name, which arrives a moment after the link is up.
const NAMED_WAIT: Duration = Duration::from_secs(5);

/// The daemon's status keys C9 names; a test ties each to the object `pods_core` writes.
const NAME_KEY: &str = "device_name";
const COUNTER_SUFFIX: &str = "_total";
/// The keys that follow the pods between the two reads.
const FOLLOW_THE_PODS: [&str; 5] = ["left", "right", "case", "headset", "lid_state"];

/// What the installed daemon showed before the run stopped it.
#[derive(Debug)]
pub(crate) enum Installed {
    /// The old daemon's line, which carries none of the keys this daemon adds.
    Old(String),
    /// Why there is no old line to compare with.
    Skipped(String),
    /// Why the installed daemon's line could not be read.
    Unread(String),
}

/// Reads the first line the installed `librepods-ctl status` prints, sample `{"case":{...},"connected":true,...}`, on the session's own runtime directory.
pub(crate) fn read(home: &OsStr, was_running: bool) -> Installed {
    if !was_running {
        return Installed::Skipped(
            "the installed daemon was not running, so it showed no line".to_owned(),
        );
    }
    let ran = match run(
        &installed_ctl(home),
        &[OsStr::new("status")],
        &[],
        STATUS_TIMEOUT,
    ) {
        Ok(ran) if ran.succeeded() => ran,
        Ok(ran) => {
            return Installed::Unread(format!(
                "the installed librepods-ctl status exited with {}",
                ran.status
            ));
        }
        Err(error) => return Installed::Unread(error),
    };
    let line = ran.stdout.lines().next().unwrap_or_default();
    let Some(object) = status_object(line) else {
        return Installed::Unread(
            "the installed librepods-ctl status printed no state object".to_owned(),
        );
    };
    let added = ADDITIVE_KEYS
        .iter()
        .filter(|key| object.contains_key(**key))
        .count();
    if added == 0 {
        Installed::Old(line.to_owned())
    } else {
        Installed::Skipped(format!(
            "the installed daemon is not the old one: its line carries {added} of the {} keys this daemon adds",
            ADDITIVE_KEYS.len()
        ))
    }
}

/// C9: every key of the old line is in the new one, and each key outside `allowance` holds the same bytes.
pub(crate) fn check(lab: &Lab<'_>) -> Result<Check, Interrupted> {
    let old = match &lab.installed {
        Installed::Old(line) => line,
        Installed::Skipped(reason) => return Ok(Check::skipped(NAME, reason.clone())),
        Installed::Unread(error) => {
            let mut check = Check::new(NAME);
            check.fail(error.clone());
            return Ok(check);
        }
    };
    let mut check = Check::new(NAME);
    let mut new = Err(String::new());
    let named = wait_until(NAMED_WAIT, || {
        new = new_line(lab);
        new.as_deref().is_ok_and(shows_a_name)
    })?;
    let new = match new {
        Ok(new) => new,
        Err(error) => {
            check.fail(error);
            return Ok(check);
        }
    };
    // Two empty names are equal bytes and would prove nothing about the escaping.
    if !named {
        check.fail(format!(
            "the daemon under test showed no name within {} s",
            NAMED_WAIT.as_secs()
        ));
    }
    if !shows_a_name(old) {
        check.fail("the installed daemon showed no name");
    }
    check.note(format!(
        "old line {} bytes, new line {} bytes",
        old.len(),
        new.len()
    ));
    for (file, line) in [(OLD_FILE, old.as_str()), (NEW_FILE, new.as_str())] {
        if let Err(error) = keep(&lab.dir.join(file), line) {
            check.fail(format!("could not keep {file}: {error}"));
        }
    }
    match compare(old, &new) {
        Ok(rows) => {
            for (key, seen) in rows {
                match (seen, allowance(&key, seen)) {
                    (Seen::Equal, _) => check.note(format!("{key}: {}", seen.word())),
                    (_, Some(reason)) => {
                        check.note(format!("{key}: {}, allowed: {reason}", seen.word()));
                    }
                    (_, None) => check.fail(format!("{key}: {}", seen.word())),
                }
            }
        }
        Err(error) => check.fail(error),
    }
    Ok(check)
}

/// The first line `librepods status` of the daemon under test prints, sample `{"advert_intake":"paused","case":{...},...}`.
fn new_line(lab: &Lab<'_>) -> Result<String, String> {
    let ran = lab.librepods(&["status"], STATUS_TIMEOUT)?;
    if !ran.succeeded() {
        return Err(format!("librepods status exited with {}", ran.status));
    }
    Ok(ran.stdout.lines().next().unwrap_or_default().to_owned())
}

/// Sample input `{"device_name":"Test\u2019s AirPods Pro",...}`: true when the name is not empty.
fn shows_a_name(line: &str) -> bool {
    status_object(line).and_then(|object| {
        object
            .get(NAME_KEY)
            .and_then(Value::as_str)
            .map(str::is_empty)
    }) == Some(false)
}

/// Writes `line` to a new file of mode 0600; an existing name, a symbolic link included, is refused and never followed.
fn keep(path: &Path, line: &str) -> Result<(), String> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC;
    let file = rustix::fs::open(path, flags, Mode::RUSR | Mode::WUSR)
        .map_err(|error| error.to_string())?;
    File::from(file)
        .write_all(line.as_bytes())
        .map_err(|error| error.to_string())
}

/// How one key compares between the two lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Equal,
    Differs,
    OnlyNew,
    OnlyOld,
}

impl Seen {
    fn word(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::Differs => "differs",
            Self::OnlyNew => "only new",
            Self::OnlyOld => "only old",
        }
    }
}

/// Why `key` may show `seen` and still pass; `device_name` is never here, because its bytes show how each daemon writes the name.
fn allowance(key: &str, seen: Seen) -> Option<&'static str> {
    match seen {
        Seen::Equal | Seen::OnlyOld => None,
        Seen::OnlyNew => ADDITIVE_KEYS
            .contains(&key)
            .then_some("the old daemon never wrote it"),
        Seen::Differs if key.ends_with(COUNTER_SUFFIX) => {
            Some("a counter counts its own daemon's calls since that daemon started")
        }
        Seen::Differs => FOLLOW_THE_PODS.contains(&key).then_some(
            "battery levels, ear state and the lid follow the pods between the two reads",
        ),
    }
}

/// Every key of either line in byte order, the two values compared as the bytes each line holds.
fn compare(old: &str, new: &str) -> Result<Vec<(String, Seen)>, String> {
    let old = raw_values(old).ok_or("the old line is not a JSON object")?;
    let new = raw_values(new).ok_or("the new line is not a JSON object")?;
    let keys: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    Ok(keys
        .into_iter()
        .map(|key| {
            let seen = match (old.get(key), new.get(key)) {
                (Some(was), Some(now)) if was.get() == now.get() => Seen::Equal,
                (Some(_), Some(_)) => Seen::Differs,
                (None, _) => Seen::OnlyNew,
                (Some(_), None) => Seen::OnlyOld,
            };
            (key.clone(), seen)
        })
        .collect())
}

/// Sample input `{"connected":true,"device_name":"Test\u2019s AirPods Pro"}`, each value kept as the bytes the line holds; the parse error is dropped, since it can quote a value.
fn raw_values(line: &str) -> Option<BTreeMap<String, &RawValue>> {
    serde_json::from_str(line).ok()
}

#[cfg(test)]
mod tests {
    use pods_core::state::State;
    use pods_core::status::to_status_object;

    use super::{COUNTER_SUFFIX, FOLLOW_THE_PODS, NAME_KEY, Seen, allowance, compare};

    #[test]
    fn every_key_c9_names_is_one_the_daemon_writes() {
        let status = to_status_object(&State::default());
        let object = status.as_object().unwrap();
        for key in FOLLOW_THE_PODS.iter().chain([&NAME_KEY]) {
            assert!(object.contains_key(*key), "{key}");
        }
        assert!(object.keys().any(|key| key.ends_with(COUNTER_SUFFIX)));
    }

    #[test]
    fn an_escaped_mark_differs_from_the_raw_one() {
        let rows = compare(
            // A raw string, so the old line holds the six characters of the JSON escape.
            r#"{"device_name":"A\u2019s","lid_state":0}"#,
            "{\"device_name\":\"A\u{2019}s\",\"lid_state\":0,\"advert_intake\":\"paused\"}",
        );
        assert_eq!(
            rows,
            Ok(vec![
                ("advert_intake".to_owned(), Seen::OnlyNew),
                ("device_name".to_owned(), Seen::Differs),
                ("lid_state".to_owned(), Seen::Equal),
            ])
        );
    }

    #[test]
    fn only_the_listed_keys_may_differ() {
        assert!(allowance("device_name", Seen::Differs).is_none());
        assert!(allowance("connected", Seen::Differs).is_none());
        assert!(allowance("left", Seen::Differs).is_some());
        assert!(allowance("connect_calls_total", Seen::Differs).is_some());
        assert!(allowance("audio_server", Seen::OnlyNew).is_some());
        assert!(allowance("noise_mode", Seen::OnlyNew).is_none());
        assert!(allowance("noise_mode", Seen::OnlyOld).is_none());
    }

    #[test]
    fn a_line_that_is_not_an_object_names_no_value() {
        let error = compare(r#""Test name""#, "{}").err().unwrap_or_default();
        assert_eq!(error, "the old line is not a JSON object");
    }
}
