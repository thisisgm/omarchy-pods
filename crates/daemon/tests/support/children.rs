//! What `/proc` says about the processes a daemon started: the ones still its children, and the ones it waited for.

use std::fs;

use rustix::io::Errno;
use rustix::process::Pid;

use super::wait_for;

/// Where `cminflt`, the minor faults of the children a process waited for, sits among the `/proc/<pid>/stat` fields after the name.
const REAPED_FAULTS_AFTER_NAME: usize = 8;

/// The one child of `pid`, waited for until it has exactly one.
pub(crate) fn the_only_child(pid: Pid) -> i32 {
    wait_for("the daemon's one child", || {
        match children(pid).as_slice() {
            [child] => Some(*child),
            _ => None,
        }
    })
}

/// The pids of every child of `pid` not yet waited for, a zombie included.
pub(crate) fn children(pid: Pid) -> Vec<i32> {
    let pid = pid.as_raw_nonzero();
    let mut found = Vec::new();
    // Each thread lists only the processes it started, so every thread is read.
    for task in fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
        let path = task.unwrap().path().join("children");
        // Sample input: `4242 4243 `, the pids of the thread's children, each followed by a space.
        let listed = match fs::read_to_string(&path) {
            Ok(listed) => listed,
            // A thread that ended after the listing has nothing left to list.
            Err(error)
                if matches!(
                    Errno::from_io_error(&error),
                    Some(Errno::NOENT | Errno::SRCH)
                ) =>
            {
                continue;
            }
            Err(error) => panic!("{}: {error}", path.display()),
        };
        for child in listed.split_whitespace() {
            let child = child
                .parse()
                .unwrap_or_else(|error| panic!("{}: `{child}`: {error}", path.display()));
            found.push(child);
        }
    }
    found
}

/// The minor faults of every child `pid` waited for, above zero once it ran any program; sample input `4242 (pods) S 4200 4242 4200 0 -1 4194560 1534 0 0 0 3 1 0 0 20 0 9 0 81234`.
pub(crate) fn reaped_children_faults(pid: Pid) -> u64 {
    let pid = pid.as_raw_nonzero();
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    // The name may hold spaces and parentheses, so the fields are counted from its last closing parenthesis.
    let (_, after_name) = stat
        .rsplit_once(')')
        .unwrap_or_else(|| panic!("no name in {stat:?}"));
    let field = after_name
        .split_whitespace()
        .nth(REAPED_FAULTS_AFTER_NAME)
        .unwrap_or_else(|| panic!("no cminflt in {stat:?}"));
    field
        .parse()
        .unwrap_or_else(|error| panic!("cminflt {field:?} in {stat:?}: {error}"))
}
