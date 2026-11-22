//! Readers of what the system reports about the link: the test unit's journal, `BlueZ`, `PipeWire` and `/proc`.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::time::Duration;

use pods_core::link::ladder::Attempt;
use pods_core::link::{Phase, STATE_POPULATED_LINE, phase_change_line};
use pods_proto::addr::BdAddr;
use serde_json::Value;

use super::Interrupted;
use super::lab::wait_until;
use super::unit::run;

/// Every phase line the daemon writes starts so; only `link: <word> to <word>` lines are ever printed.
pub(super) const PHASE_PREFIX: &str = "link: ";
/// `MESSAGE_ID` of the journal line systemd writes when a unit has started.
pub(super) const UNIT_STARTED_ID: &str = "39f53479d3a045ac8e11786248231fbf";
/// The service UUID of the accessory protocol, which marks a `BlueZ` device as `AirPods`.
pub(super) const AAP_UUID: &str = "74ec2172-0bad-4d01-8f77-997b2be0722a";
/// How long a read of the journal, `BlueZ` or `PipeWire` may take.
pub(super) const READ_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const MICROS_PER_MILLI: u64 = 1000;

/// One journal line of the test unit: its monotonic stamp, its text and its message id.
pub(super) struct Entry {
    pub(super) micros: u64,
    pub(super) message: String,
    pub(super) message_id: Option<String>,
    pub(super) cursor: String,
}

/// One line of `journalctl --output=json`; a message that is not text is kept empty; sample input `{"__CURSOR":"s=1;i=2","__MONOTONIC_TIMESTAMP":"812345678","MESSAGE":"link: state populated"}`.
pub(super) fn parse_entry(line: &str) -> Option<Entry> {
    let Value::Object(object) = serde_json::from_str(line).ok()? else {
        return None;
    };
    let text = |key: &str| object.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(Entry {
        micros: text("__MONOTONIC_TIMESTAMP")?.parse().ok()?,
        message: text("MESSAGE").unwrap_or_default(),
        message_id: text("MESSAGE_ID"),
        cursor: text("__CURSOR")?,
    })
}

pub(super) fn journalctl(extra: &[OsString]) -> Result<Vec<Entry>, String> {
    let mut args: Vec<OsString> = [
        "--user",
        "--unit=librepods-hw.service",
        "--boot=0",
        "--output=json",
        "--output-fields=MESSAGE,MESSAGE_ID",
        "--no-pager",
        "--quiet",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.extend_from_slice(extra);
    let refs: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
    let ran = run("journalctl", &refs, &[], READ_TIMEOUT)?;
    if !ran.succeeded() {
        return Err(format!("journalctl exited with {}", ran.status));
    }
    Ok(ran.stdout.lines().filter_map(parse_entry).collect())
}

/// The cursor of the test unit's last journal line, so a later read sees only what came after.
pub(super) fn journal_cursor() -> Result<Option<String>, String> {
    Ok(journalctl(&["--lines=1".into()])?
        .pop()
        .map(|entry| entry.cursor))
}

pub(super) fn journal_after(cursor: Option<&str>) -> Result<Vec<Entry>, String> {
    match cursor {
        Some(cursor) => journalctl(&[format!("--after-cursor={cursor}").into()]),
        None => journalctl(&[]),
    }
}

/// How a wait on the journal ended.
pub(super) enum Waited {
    Found(Vec<Entry>),
    TimedOut,
    ReadFailed(String),
}

/// Reads the journal after `cursor` until `done` holds for its lines, for at most `timeout`; a failed read ends the wait.
pub(super) fn wait_for_entries(
    cursor: Option<&str>,
    timeout: Duration,
    done: impl Fn(&[Entry]) -> bool,
) -> Result<Waited, Interrupted> {
    let mut waited = Waited::TimedOut;
    wait_until(timeout, || match journal_after(cursor) {
        Ok(entries) if done(&entries) => {
            waited = Waited::Found(entries);
            true
        }
        Ok(_) => false,
        Err(error) => {
            waited = Waited::ReadFailed(error);
            true
        }
    })?;
    Ok(waited)
}

/// The index of the first line at or after `from` whose message contains `text`.
pub(super) fn find(entries: &[Entry], text: &str, from: usize) -> Option<usize> {
    entries
        .iter()
        .skip(from)
        .position(|entry| entry.message.contains(text))?
        .checked_add(from)
}

/// The journal line the daemon writes when `BlueZ` reports the device connected and it opens the channel.
pub(super) fn opening_line() -> String {
    let opening = Phase::Opening {
        attempt: Attempt::Held(1),
    };
    phase_change_line(Phase::Idle, opening)
}

/// Milliseconds between two journal lines, by the journal's own monotonic stamps.
pub(super) fn between(entries: &[Entry], first: usize, last: usize) -> Option<u64> {
    let first = entries.get(first)?.micros;
    let last = entries.get(last)?.micros;
    last.checked_sub(first)?.checked_div(MICROS_PER_MILLI)
}

/// The phase lines between two journal lines, each with its offset; only `link: <Word> to <Word>` text is kept.
pub(super) fn phase_lines(entries: &[Entry], first: usize, last: usize) -> Vec<String> {
    let Some(start) = entries.get(first).map(|entry| entry.micros) else {
        return Vec::new();
    };
    let span = entries.iter().take(last.saturating_add(1)).skip(first);
    span.filter(|entry| is_phase_line(&entry.message))
        .map(|entry| {
            let offset = entry
                .micros
                .saturating_sub(start)
                .checked_div(MICROS_PER_MILLI)
                .unwrap_or(0);
            format!("+{offset} ms {}", entry.message)
        })
        .collect()
}

/// True for `link: opening to handshake` and `link: state populated`, the only daemon lines safe to print; sample input `link: idle to opening`.
pub(super) fn is_phase_line(message: &str) -> bool {
    if message == STATE_POPULATED_LINE {
        return true;
    }
    let Some(rest) = message.strip_prefix(PHASE_PREFIX) else {
        return false;
    };
    let words: Vec<&str> = rest.split(' ').collect();
    matches!(words.as_slice(), [from, "to", to]
        if [from, to].iter().all(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_alphabetic())))
}

/// The `AirPods` `BlueZ` knows and the adapters, read from one `GetManagedObjects` call.
pub(super) struct Bluez {
    pub(super) devices: Vec<Device>,
    pub(super) adapters: Vec<Adapter>,
}

impl Bluez {
    /// The first `AirPods` device `BlueZ` reports connected.
    pub(super) fn connected(self) -> Option<Device> {
        self.devices.into_iter().find(|device| device.connected)
    }
}

pub(super) struct Device {
    pub(super) path: String,
    pub(super) adapter: String,
    pub(super) connected: bool,
    /// Held in memory only; `BdAddr` prints as `BdAddr(redacted)`.
    pub(super) address: Option<BdAddr>,
}

pub(super) struct Adapter {
    pub(super) path: String,
    pub(super) powered: bool,
    pub(super) discovering: bool,
}

pub(super) fn bluez() -> Result<Bluez, String> {
    let args = [
        "--system",
        "--json=short",
        "call",
        "org.bluez",
        "/",
        "org.freedesktop.DBus.ObjectManager",
        "GetManagedObjects",
    ];
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let ran = run("busctl", &args, &[], READ_TIMEOUT)?;
    if !ran.succeeded() {
        return Err(format!(
            "busctl GetManagedObjects exited with {}",
            ran.status
        ));
    }
    parse_bluez(&ran.stdout).ok_or_else(|| "busctl printed no BlueZ object tree".to_owned())
}

/// The `AirPods` devices and the adapters in `BlueZ`'s object tree, as `busctl --json=short` prints it; sample input `{"type":"a{oa{sa{sv}}}","data":[{"/org/bluez/hci0":{"org.bluez.Adapter1":{"Powered":{"type":"b","data":true},"Discovering":{"type":"b","data":false}}},"/org/bluez/hci0/dev_02_00_00_00_00_01":{"org.bluez.Device1":{"Address":{"type":"s","data":"02:00:00:00:00:01"},"Adapter":{"type":"o","data":"/org/bluez/hci0"},"Connected":{"type":"b","data":true},"UUIDs":{"type":"as","data":["74ec2172-0bad-4d01-8f77-997b2be0722a"]}}}}]}`.
pub(super) fn parse_bluez(text: &str) -> Option<Bluez> {
    let root: Value = serde_json::from_str(text.trim()).ok()?;
    let objects = root.get("data")?.get(0)?.as_object()?;
    let mut bluez = Bluez {
        devices: Vec::new(),
        adapters: Vec::new(),
    };
    for (path, interfaces) in objects {
        let property = |interface: &str, name: &str| {
            interfaces.get(interface)?.get(name)?.get("data").cloned()
        };
        let flag = |interface: &str, name: &str| {
            property(interface, name)
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        };
        if interfaces.get("org.bluez.Adapter1").is_some() {
            bluez.adapters.push(Adapter {
                path: path.clone(),
                powered: flag("org.bluez.Adapter1", "Powered"),
                discovering: flag("org.bluez.Adapter1", "Discovering"),
            });
        }
        let uuids = property("org.bluez.Device1", "UUIDs");
        let is_airpods = uuids
            .as_ref()
            .and_then(Value::as_array)
            .is_some_and(|uuids| uuids.iter().any(|uuid| uuid == AAP_UUID));
        if is_airpods {
            bluez.devices.push(Device {
                path: path.clone(),
                adapter: property("org.bluez.Device1", "Adapter")
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                connected: flag("org.bluez.Device1", "Connected"),
                address: property("org.bluez.Device1", "Address")
                    .and_then(|value| value.as_str()?.parse().ok()),
            });
        }
    }
    Some(bluez)
}

/// Whether a Bluetooth audio card sits in a headset profile, where a connect can time out.
pub(crate) fn headset_profile() -> Result<bool, String> {
    let args = [OsStr::new("list"), OsStr::new("cards")];
    let ran = run("pactl", &args, &[("LC_ALL", OsStr::new("C"))], READ_TIMEOUT)?;
    if !ran.succeeded() {
        return Err(format!("pactl list cards exited with {}", ran.status));
    }
    Ok(in_headset_profile(&ran.stdout))
}

/// True when a `bluez_card` in `pactl list cards` text has a `headset` active profile; sample input `Card #52\n\tName: bluez_card.02_00_00_00_00_01\n\tActive Profile: headset-head-unit\n`.
pub(super) fn in_headset_profile(text: &str) -> bool {
    let mut in_bluez_card = false;
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix("Name: ") {
            in_bluez_card = name.starts_with("bluez_card.");
        } else if let Some(profile) = line.strip_prefix("Active Profile: ")
            && in_bluez_card
            && profile.starts_with("headset")
        {
            return true;
        }
    }
    false
}

/// The voluntary context switches of a process's threads since the first sample, which is how often it woke.
pub(super) struct Wakeups {
    pid: u32,
    /// Per thread id: its count at the first sample that saw it, 0 for a thread born later, and at the latest sample.
    threads: BTreeMap<u32, (u64, u64)>,
}

impl Wakeups {
    pub(super) fn start(pid: u32) -> Result<Self, String> {
        let threads = thread_switches(pid)?
            .into_iter()
            .map(|(thread, count)| (thread, (count, count)))
            .collect();
        Ok(Self { pid, threads })
    }

    /// Reads every thread again; a thread that has ended keeps the count of the last sample that saw it.
    pub(super) fn sample(&mut self) -> Result<(), String> {
        for (thread, count) in thread_switches(self.pid)? {
            self.threads
                .entry(thread)
                .and_modify(|counts| counts.1 = count)
                .or_insert((0, count));
        }
        Ok(())
    }

    /// The wakeups of one thread, so a test can pin the thread it made.
    #[cfg(test)]
    pub(super) fn woke_by(&self, thread: u32) -> u64 {
        self.threads
            .get(&thread)
            .map_or(0, |(first, latest)| latest.saturating_sub(*first))
    }

    pub(super) fn woke(&self) -> u64 {
        self.threads
            .values()
            .map(|(first, latest)| latest.saturating_sub(*first))
            .fold(0, u64::saturating_add)
    }
}

/// Each thread of `pid` with its voluntary context switches, by the thread's `/proc/<pid>/task` entry; sample input entry name `4243`.
fn thread_switches(pid: u32) -> Result<Vec<(u32, u64)>, String> {
    let tasks = fs::read_dir(format!("/proc/{pid}/task"))
        .map_err(|error| format!("/proc/{pid}/task: {error}"))?;
    let mut threads = Vec::new();
    for task in tasks {
        let task = task.map_err(|error| format!("/proc/{pid}/task: {error}"))?;
        let name = task.file_name();
        let thread = name
            .to_str()
            .and_then(|name| name.parse().ok())
            .ok_or_else(|| format!("/proc/{pid}/task holds an entry that is not a thread id"))?;
        // A thread that ended between the listing and the read has nothing left to count.
        let Ok(status) = fs::read_to_string(task.path().join("status")) else {
            continue;
        };
        let count =
            voluntary_switches(&status).ok_or("a task status has no voluntary_ctxt_switches")?;
        threads.push((thread, count));
    }
    Ok(threads)
}

/// The calling thread's id, from the `/proc/thread-self` link; sample input link target `4242/task/4243`.
#[cfg(test)]
pub(super) fn current_thread_id() -> u32 {
    let target = fs::read_link("/proc/thread-self").unwrap();
    target
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// The `voluntary_ctxt_switches` field of a `/proc/<pid>/task/<tid>/status` file; sample input `voluntary_ctxt_switches:\t1234\n`.
pub(super) fn voluntary_switches(status: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        line.strip_prefix("voluntary_ctxt_switches:")?
            .trim()
            .parse()
            .ok()
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;
    use std::{process, thread};

    use pods_proto::addr::BdAddr;
    use proptest::prelude::*;

    use super::super::lab::status_object;
    use super::super::unit::pause;
    use super::{
        Wakeups, current_thread_id, in_headset_profile, is_phase_line, parse_bluez, parse_entry,
        voluntary_switches,
    };

    /// How many times the thread under count sleeps, so wakes, before it ends.
    const SLEEPS: u64 = 200;
    /// How many 1 ms looks a test takes for an ended thread to leave `/proc`.
    const GONE_POLLS: u32 = 1000;

    #[test]
    pub(super) fn a_journal_line_gives_its_stamp_text_and_id() {
        let entry = parse_entry(r#"{"__CURSOR":"c3","__MONOTONIC_TIMESTAMP":"812345678","MESSAGE":"link: state populated","MESSAGE_ID":"x"}"#).unwrap();
        assert_eq!(
            (
                entry.micros,
                entry.message.as_str(),
                entry.message_id.as_deref(),
                entry.cursor.as_str()
            ),
            (812_345_678, "link: state populated", Some("x"), "c3")
        );
        assert!(parse_entry(r#"{"__CURSOR":"c3","MESSAGE":"x"}"#).is_none());
    }

    #[test]
    pub(super) fn only_bare_phase_lines_are_printable() {
        assert!(is_phase_line("link: idle to opening"));
        assert!(is_phase_line("link: state populated"));
        assert!(!is_phase_line("link: peer 02:00:00:00:00:01 to opening"));
        assert!(!is_phase_line(
            "link: idle to opening dev_02_00_00_00_00_01"
        ));
    }

    #[test]
    pub(super) fn bluez_objects_give_airpods_and_adapters() {
        let text = r#"{"type":"a{oa{sa{sv}}}","data":[{"/org/bluez/hci0":{"org.bluez.Adapter1":{"Powered":{"type":"b","data":true},"Discovering":{"type":"b","data":false}}},"/org/bluez/hci0/dev_02_00_00_00_00_01":{"org.bluez.Device1":{"Address":{"type":"s","data":"02:00:00:00:00:01"},"Adapter":{"type":"o","data":"/org/bluez/hci0"},"Connected":{"type":"b","data":true},"UUIDs":{"type":"as","data":["74ec2172-0bad-4d01-8f77-997b2be0722a"]}}},"/org/bluez/hci0/dev_02_00_00_00_00_02":{"org.bluez.Device1":{"Connected":{"type":"b","data":true},"UUIDs":{"type":"as","data":[]}}}}]}"#;
        let bluez = parse_bluez(text).unwrap();
        assert_eq!(bluez.devices.len(), 1);
        assert!(bluez.devices[0].connected);
        assert_eq!(bluez.devices[0].adapter, "/org/bluez/hci0");
        assert_eq!(
            bluez.devices[0].address,
            Some(BdAddr::from([0x02, 0, 0, 0, 0, 0x01]))
        );
        assert!(bluez.adapters[0].powered && !bluez.adapters[0].discovering);
    }

    #[test]
    pub(super) fn only_a_bluetooth_card_in_a_headset_profile_counts() {
        assert!(in_headset_profile(
            "Card #1\n\tName: bluez_card.02_00_00_00_00_01\n\tActive Profile: headset-head-unit\n"
        ));
        assert!(!in_headset_profile(
            "Card #1\n\tName: bluez_card.02_00_00_00_00_01\n\tActive Profile: a2dp-sink\n"
        ));
        assert!(!in_headset_profile(
            "Card #0\n\tName: alsa_card.pci\n\tActive Profile: headset-x\n"
        ));
    }

    #[test]
    pub(super) fn a_thread_that_ends_inside_the_window_is_still_counted() {
        let (slept, slept_seen) = mpsc::channel();
        let (end, ended) = mpsc::channel::<()>();
        let mut wakeups = Wakeups::start(process::id()).unwrap();
        let sleeper = thread::spawn(move || {
            for _ in 0..SLEEPS {
                pause(Duration::from_millis(1));
            }
            slept.send(current_thread_id()).unwrap();
            ended.recv().unwrap();
        });
        let sleeper_id = slept_seen.recv().unwrap();
        wakeups.sample().unwrap();
        end.send(()).unwrap();
        sleeper.join().unwrap();
        // A joined thread can stay listed in `/proc` for a moment; the sample must find it gone.
        let listed = format!("/proc/{}/task/{sleeper_id}", process::id());
        let gone = (0..GONE_POLLS).any(|_| {
            pause(Duration::from_millis(1));
            !Path::new(&listed).exists()
        });
        assert!(gone, "the joined thread is still listed");
        wakeups.sample().unwrap();
        let woke = wakeups.woke_by(sleeper_id);
        assert!(woke >= SLEEPS, "{woke} wakeups");
        assert!(wakeups.woke() >= woke);
    }

    #[test]
    pub(super) fn context_switches_are_read_from_a_task_status() {
        let status = "Name:\tx\nvoluntary_ctxt_switches:\t1234\nnonvoluntary_ctxt_switches:\t5\n";
        assert_eq!(voluntary_switches(status), Some(1234));
    }

    proptest! {
        /// Every parser of a tool's output takes the lossy text of any bytes without a panic, as `read_output` hands it over.
        #[test]
        fn every_tool_output_parser_takes_any_bytes(
            bytes in proptest::collection::vec(any::<u8>(), 0..=4096)
        ) {
            let text = String::from_utf8_lossy(&bytes);
            for line in text.lines() {
                let _entry = parse_entry(line);
                let _phase = is_phase_line(line);
            }
            let _bluez = parse_bluez(&text);
            let _headset = in_headset_profile(&text);
            let _switches = voluntary_switches(&text);
            let _status = status_object(&text);
        }
    }
}
