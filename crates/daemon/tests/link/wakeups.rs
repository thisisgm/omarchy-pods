//! An open link is read on the daemon's first thread: once it is up the daemon keeps no thread but that one and tokio's blocking pool, and a report from the pods that changes nothing wakes that thread and no other.

use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use pods_fake::Profile;
use pods_samples::{CASE, LEFT, RIGHT, host, metadata};
use rustix::process::Pid;

use crate::support::uplink::rig_with;
use crate::support::{HANG_LIMIT, pause, wait_for};

/// Reports sent, each the same as the profile's own, so none changes the state.
const REPORTS: usize = 20;

/// The daemon counts as settled once no thread of it slept or started for this long.
const QUIET: Duration = Duration::from_millis(200);

/// tokio's default name for the threads of its blocking pool, which the daemon leaves as it is; the state file's writes run on them and they stay for a while after a write.
const POOL_THREAD: &str = "tokio-rt-worker";

/// How often each thread of `pid` went to sleep of its own accord, by thread id, from its `/proc/<pid>/task/<thread>/status`; sample input line `voluntary_ctxt_switches:\t1234`.
fn sleeps(pid: Pid) -> BTreeMap<String, u64> {
    let tasks = fs::read_dir(format!("/proc/{}/task", pid.as_raw_nonzero())).unwrap();
    let mut sleeps = BTreeMap::new();
    for task in tasks {
        let task = task.unwrap();
        // A thread that ended between the listing and the read has nothing left to count.
        let Ok(status) = fs::read_to_string(task.path().join("status")) else {
            continue;
        };
        let count = status
            .lines()
            .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        sleeps.insert(task.file_name().into_string().unwrap(), count);
    }
    sleeps
}

/// The name of thread `thread` of `pid`, from its `/proc/<pid>/task/<thread>/comm`, or `None` once it has ended; sample input `tokio-rt-worker\n`.
fn name(pid: Pid, thread: &str) -> Option<String> {
    let comm = format!("/proc/{}/task/{thread}/comm", pid.as_raw_nonzero());
    let name = fs::read_to_string(comm).ok()?;
    Some(name.trim_end().to_owned())
}

#[test]
fn a_report_that_changes_nothing_wakes_only_the_first_thread() {
    let (mut rig, device) = rig_with("off", &[], Profile::default());
    rig.link_up(&device);
    rig.wait_filled();
    let request = host::request_proximity_keys();
    // The daemon asks for the keys once for each metadata and at no other time, and its answer to the link's own may come after the status filled.
    let asked = device
        .pods
        .wait_heard(&request, 1, HANG_LIMIT)
        .unwrap()
        .len();
    let pid = rig.daemon().pid();
    // Linux gives a process's first thread the process's own id.
    let first = pid.as_raw_nonzero().to_string();
    let mut last = sleeps(pid);
    let settled = wait_for("the daemon to settle after the link came up", || {
        pause(QUIET);
        let now = sleeps(pid);
        let quiet = now == last;
        last = now;
        quiet.then(|| last.clone())
    });
    // A pool thread that read the channel would show in the count below, so only its name is let through here.
    let strays: Vec<String> = settled
        .keys()
        .filter(|thread| **thread != first)
        .filter_map(|thread| name(pid, thread))
        .filter(|name| name != POOL_THREAD)
        .collect();
    assert_eq!(
        strays,
        Vec::<String>::new(),
        "threads other than the first and the blocking pool's, by name"
    );
    let mut slept = settled[&first];
    for report in 1..=REPORTS {
        device.pods.set_battery(LEFT, RIGHT, CASE);
        // A timer of the daemon ends this wait as well as a report does, so the wait only spaces the reports.
        slept = wait_for(
            &format!("the first thread to sleep after report {report}"),
            || {
                let now = sleeps(pid)[&first];
                (now > slept).then_some(now)
            },
        );
    }
    // One more keys request answers this metadata, so it shows the daemon has read every report sent before.
    device.pods.send(&metadata());
    device
        .pods
        .wait_heard(&request, asked + 1, HANG_LIMIT)
        .unwrap();
    let mut woken = sleeps(pid);
    woken.remove(&first);
    // A thread that has ended since is gone from `woken`, and one that woke or started differs from `settled`.
    woken.retain(|thread, count| settled.get(thread) != Some(count));
    assert_eq!(
        woken,
        BTreeMap::new(),
        "threads other than the first that slept or started during {REPORTS} reports"
    );
    rig.finish();
}
