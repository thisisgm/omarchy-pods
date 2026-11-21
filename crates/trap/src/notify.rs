//! The four ioctls of a seccomp listener, each behind a safe function.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

use libc::{seccomp_data, seccomp_notif, seccomp_notif_addfd, seccomp_notif_resp};
use rustix::io::Errno;

/// `#define SECCOMP_USER_NOTIF_FLAG_CONTINUE (1UL << 0)` in linux/seccomp.h.
const FLAG_CONTINUE: u32 = 1 << 0;
/// `#define SECCOMP_ADDFD_FLAG_SEND (1UL << 1)` in linux/seccomp.h.
const ADDFD_FLAG_SEND: u32 = 1 << 1;

/// One trapped system call: `tid` is the calling thread in this process's PID namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Call {
    pub(crate) id: u64,
    pub(crate) tid: u32,
    pub(crate) number: i64,
    pub(crate) args: [u64; 6],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reply {
    Value(i64),
    Error(Errno),
    /// Let the kernel run the call as the child made it.
    Continue,
}

fn last_errno() -> Errno {
    Errno::from_io_error(&io::Error::last_os_error()).unwrap_or(Errno::IO)
}

pub(crate) fn receive(listener: BorrowedFd<'_>) -> Result<Call, Errno> {
    // The kernel refuses a buffer that is not all zero.
    let mut notification = seccomp_notif {
        id: 0,
        pid: 0,
        flags: 0,
        data: seccomp_data {
            nr: 0,
            arch: 0,
            instruction_pointer: 0,
            args: [0; 6],
        },
    };
    #[expect(
        unsafe_code,
        reason = "the libc crate is the only binding of the seccomp ioctls"
    )]
    // SAFETY: the listener is open for the call, and the kernel writes one `struct seccomp_notif` into `notification`, which lives on this stack frame and has that exact layout.
    let result = unsafe {
        libc::ioctl(
            listener.as_raw_fd(),
            libc::SECCOMP_IOCTL_NOTIF_RECV,
            &raw mut notification,
        )
    };
    if result < 0 {
        return Err(last_errno());
    }
    Ok(Call {
        id: notification.id,
        tid: notification.pid,
        number: i64::from(notification.data.nr),
        args: notification.data.args,
    })
}

/// False once the caller has gone, which makes anything read from its memory stale.
pub(crate) fn still_valid(listener: BorrowedFd<'_>, id: u64) -> bool {
    #[expect(
        unsafe_code,
        reason = "the libc crate is the only binding of the seccomp ioctls"
    )]
    // SAFETY: the listener is open for the call, and the kernel reads one `u64` from `id`, which lives on this stack frame.
    let result = unsafe {
        libc::ioctl(
            listener.as_raw_fd(),
            libc::SECCOMP_IOCTL_NOTIF_ID_VALID,
            &raw const id,
        )
    };
    result == 0
}

/// `Err(Errno::NOENT)` means the caller was killed before the answer reached it.
pub(crate) fn answer(listener: BorrowedFd<'_>, id: u64, reply: Reply) -> Result<(), Errno> {
    let (val, error, flags) = match reply {
        Reply::Value(value) => (value, 0, 0),
        Reply::Error(errno) => (0, errno.raw_os_error().wrapping_neg(), 0),
        Reply::Continue => (0, 0, FLAG_CONTINUE),
    };
    let response = seccomp_notif_resp {
        id,
        val,
        error,
        flags,
    };
    #[expect(
        unsafe_code,
        reason = "the libc crate is the only binding of the seccomp ioctls"
    )]
    // SAFETY: the listener is open for the call, and the kernel reads one `struct seccomp_notif_resp` from `response`, which lives on this stack frame and has that exact layout.
    let result = unsafe {
        libc::ioctl(
            listener.as_raw_fd(),
            libc::SECCOMP_IOCTL_NOTIF_SEND,
            &raw const response,
        )
    };
    if result < 0 {
        Err(last_errno())
    } else {
        Ok(())
    }
}

/// Puts a copy of `descriptor` into the caller with `O_CLOEXEC` and returns its number there as the call's result, in one step.
pub(crate) fn answer_with_descriptor(
    listener: BorrowedFd<'_>,
    id: u64,
    descriptor: BorrowedFd<'_>,
) -> Result<u32, Errno> {
    let addfd = seccomp_notif_addfd {
        id,
        flags: ADDFD_FLAG_SEND,
        srcfd: descriptor.as_raw_fd().cast_unsigned(),
        newfd: 0,
        newfd_flags: libc::O_CLOEXEC.cast_unsigned(),
    };
    #[expect(
        unsafe_code,
        reason = "the libc crate is the only binding of the seccomp ioctls"
    )]
    // SAFETY: the listener and `descriptor` are open for the call, and the kernel reads one `struct seccomp_notif_addfd` from `addfd`, which lives on this stack frame and has that exact layout.
    let result = unsafe {
        libc::ioctl(
            listener.as_raw_fd(),
            libc::SECCOMP_IOCTL_NOTIF_ADDFD,
            &raw const addfd,
        )
    };
    u32::try_from(result).ok().ok_or_else(last_errno)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_local_flags_are_the_kernel_flags() {
        assert_eq!(
            u64::from(FLAG_CONTINUE),
            libc::SECCOMP_USER_NOTIF_FLAG_CONTINUE
        );
        assert_eq!(u64::from(ADDFD_FLAG_SEND), libc::SECCOMP_ADDFD_FLAG_SEND);
    }
}
