//! Standard input at a cue of a step file: read as it arrives, never waited on.

use std::io;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;

/// The most bytes one read takes; a longer line is read in parts, and the part that ends it is the answer.
pub(crate) const READ_LEN: usize = 256;
/// A poll that returns at once.
const NO_WAIT: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 0,
};
/// The most reads that take away what was typed before a cue; input still coming after them answers the cue.
const MAX_DISCARD_READS: usize = 64;

/// Why a cue got no line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum NoLine {
    #[error("standard input ended at a cue, which waits for a line")]
    Ended,
    #[error("standard input could not be read at a cue: {0}")]
    Failed(Errno),
}

/// What one read of standard input took, without waiting: `None` for nothing, else whether the end of a line was in it.
// Sample input: `\n` or `yes\n`; whatever the line holds, its end is the answer.
fn look() -> Result<Option<bool>, NoLine> {
    let stdin = io::stdin();
    let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
    match poll(&mut fds, Some(&NO_WAIT)) {
        Ok(0) | Err(Errno::INTR) => return Ok(None),
        Ok(_) => {}
        Err(errno) => return Err(NoLine::Failed(errno)),
    }
    let mut bytes = [0_u8; READ_LEN];
    match rustix::io::read(&stdin, &mut bytes) {
        Ok(0) => Err(NoLine::Ended),
        Ok(len) => Ok(bytes.get(..len).map(|read| read.contains(&b'\n'))),
        Err(Errno::INTR | Errno::AGAIN) => Ok(None),
        Err(errno) => Err(NoLine::Failed(errno)),
    }
}

/// Whether the end of a line arrived on standard input since the last call, in at most one read, which never waits.
pub(super) fn line_arrived() -> Result<bool, NoLine> {
    Ok(look()? == Some(true))
}

/// Takes away what is on standard input before a cue is shown, so none of it answers the cue; an input that has ended says so at the cue's first look.
pub(super) fn discard_typed_ahead() {
    for _ in 0..MAX_DISCARD_READS {
        if !matches!(look(), Ok(Some(_))) {
            return;
        }
    }
}
