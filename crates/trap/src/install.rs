//! Starts the child under the filter and takes its listener back over a socket pair.

use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::time::Instant;

use libc::{
    SECCOMP_FILTER_FLAG_NEW_LISTENER, SECCOMP_FILTER_FLAG_WAIT_KILLABLE_RECV,
    SECCOMP_GET_ACTION_AVAIL, SECCOMP_RET_USER_NOTIF, SECCOMP_SET_MODE_FILTER, SYS_seccomp,
    sock_filter, sock_fprog,
};
use rustix::cmsg_space;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, SocketFlags, SocketType, recvmsg, sendmsg, socketpair,
};

use crate::filter::PROGRAM;
use crate::record::{Route, TrapError};
use crate::{HAND_OVER_WAIT, Trap};

/// Starts `command` with every Bluetooth `socket` and L2CAP `connect` call of it answered by the returned trap; the routes hold before the child's first call.
pub fn spawn(mut command: Command, routes: Vec<Route>) -> Result<(Child, Trap), TrapError> {
    if !user_notification_available() {
        return Err(TrapError::Unsupported);
    }
    let (test_end, child_end) = socketpair(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|errno| TrapError::Spawn(errno.into()))?;
    #[expect(
        unsafe_code,
        reason = "the filter must be installed in the child between fork and exec"
    )]
    // SAFETY: `install_filter` allocates nothing, takes no lock and cannot panic; it makes only the system calls prctl, seccomp and sendmsg, which are async signal safe.
    unsafe {
        command.pre_exec(move || install_filter(&child_end));
    }
    let spawned = Instant::now();
    let started = command.spawn();
    // Closes the test's copy of the child's end, so a child that never sends hangs up the pair.
    drop(command);
    let mut child = started.map_err(TrapError::Spawn)?;
    let trap = match receive_listener(&test_end) {
        Some(listener) => Trap::start(listener, routes, spawned)
            .map_err(|error| TrapError::Supervisor(format!("thread spawn failed: {error}"))),
        None => Err(TrapError::HandOver),
    };
    match trap {
        Ok(trap) => Ok((child, trap)),
        Err(error) => {
            // Already gone is the only way a kill fails here, and the wait then reaps it.
            let _killed = child.kill();
            let _reaped = child.wait();
            Err(error)
        }
    }
}

fn user_notification_available() -> bool {
    let action: u32 = SECCOMP_RET_USER_NOTIF;
    #[expect(
        unsafe_code,
        reason = "the libc crate is the only binding of the seccomp system call"
    )]
    // SAFETY: `SECCOMP_GET_ACTION_AVAIL` reads one `u32` from the pointer, which points at `action` on this stack frame.
    let result =
        unsafe { libc::syscall(SYS_seccomp, SECCOMP_GET_ACTION_AVAIL, 0, &raw const action) };
    result == 0
}

/// Runs in the child between fork and exec: no allocation, no lock, no panic.
fn install_filter(child_end: &OwnedFd) -> io::Result<()> {
    rustix::thread::set_no_new_privs(true)?;
    let filter: &'static [sock_filter] = &PROGRAM;
    let Ok(len) = u16::try_from(filter.len()) else {
        return Err(Errno::INVAL.into());
    };
    let program = sock_fprog {
        len,
        filter: filter.as_ptr().cast_mut(),
    };
    let flags = SECCOMP_FILTER_FLAG_NEW_LISTENER | SECCOMP_FILTER_FLAG_WAIT_KILLABLE_RECV;
    #[expect(
        unsafe_code,
        reason = "the libc crate is the only binding of the seccomp system call"
    )]
    // SAFETY: the kernel reads one `struct sock_fprog` from `program`, on this stack frame, and `len` instructions from `PROGRAM`, which the borrow above promotes to a static; it writes nothing.
    let listener = unsafe {
        libc::syscall(
            SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            flags,
            &raw const program,
        )
    };
    let Ok(listener) = i32::try_from(listener) else {
        return Err(Errno::BADF.into());
    };
    if listener < 0 {
        return Err(io::Error::last_os_error());
    }
    #[expect(
        unsafe_code,
        reason = "rustix sends only borrowed descriptors and seccomp returns a bare number"
    )]
    // SAFETY: the kernel just opened `listener` in this process and nothing closes it before exec, which ends this borrow.
    let listener = [unsafe { BorrowedFd::borrow_raw(listener) }];
    let mut space = [MaybeUninit::uninit(); cmsg_space!(ScmRights(1))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    if !ancillary.push(SendAncillaryMessage::ScmRights(&listener)) {
        return Err(Errno::NOBUFS.into());
    }
    sendmsg(
        child_end,
        &[IoSlice::new(&[0])],
        &mut ancillary,
        SendFlags::empty(),
    )?;
    Ok(())
}

/// The listener the child sent, or `None` when nothing came within `HAND_OVER_WAIT`.
fn receive_listener(test_end: &OwnedFd) -> Option<OwnedFd> {
    let timeout = Timespec::try_from(HAND_OVER_WAIT).ok()?;
    let ready = poll(&mut [PollFd::new(test_end, PollFlags::IN)], Some(&timeout));
    if !matches!(ready, Ok(1)) {
        return None;
    }
    let mut byte = [0];
    let mut space = [MaybeUninit::uninit(); cmsg_space!(ScmRights(1))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut space);
    let flags = RecvFlags::CMSG_CLOEXEC | RecvFlags::DONTWAIT;
    recvmsg(
        test_end,
        &mut [IoSliceMut::new(&mut byte)],
        &mut ancillary,
        flags,
    )
    .ok()?;
    ancillary.drain().find_map(|message| {
        if let RecvAncillaryMessage::ScmRights(mut descriptors) = message {
            descriptors.next()
        } else {
            None
        }
    })
}
