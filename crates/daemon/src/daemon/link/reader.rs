//! The open channel on the runtime's own thread: each SDU as it comes, then the channel's end.

use std::io;

use pods_sys::l2cap::{Channel, Received};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::task;
use zeroize::{Zeroize, Zeroizing};

use crate::daemon::log;

/// From aacp.rs in librepods-org/librepods pull request 655; a longer SDU is dropped.
pub(super) const RECV_BUFFER_LEN: usize = 4096;

/// An SDU, a hang up and a socket error each make the channel worth a read.
const WORTH_A_READ: Interest = Interest::READABLE.add(Interest::ERROR);

#[derive(Debug)]
pub(in crate::daemon) struct Open {
    channel: AsyncFd<Channel>,
    buf: Box<[u8]>,
    truncation_logged: bool,
}

impl Open {
    /// Hands the socket to the runtime's poller, so it must be called on the runtime.
    #[expect(
        deprecated,
        reason = "the replacement is an unsafe fn and this crate forbids unsafe; a `Channel` owns its one descriptor for life, which is all that fn asks its caller to promise"
    )]
    pub(in crate::daemon) fn watch(channel: Channel) -> io::Result<Self> {
        Ok(Self {
            channel: AsyncFd::with_interest(channel, WORTH_A_READ)?,
            buf: vec![0; RECV_BUFFER_LEN].into(),
            truncation_logged: false,
        })
    }

    pub(in crate::daemon) fn channel(&self) -> &Channel {
        self.channel.get_ref()
    }

    /// The next SDU, or `None` once the channel has ended, whichever side ended it; no await sits between taking an SDU and returning it, so dropping this future loses none.
    pub(in crate::daemon) async fn read(&mut self) -> Option<Zeroizing<Box<[u8]>>> {
        loop {
            let mut ready = match self.channel.ready(WORTH_A_READ).await {
                Ok(ready) => ready,
                Err(error) => {
                    log(&format!("link: cannot wait on the channel: {error}"));
                    return None;
                }
            };
            match ready.get_inner().try_recv(&mut self.buf) {
                Ok(Received::Sdu(len)) => {
                    let used = self.buf.get_mut(..len).unwrap_or_default();
                    let sdu = Zeroizing::new(Box::from(&*used));
                    used.zeroize();
                    return Some(sdu);
                }
                Ok(Received::Truncated { full_len }) => {
                    self.buf.zeroize();
                    if !self.truncation_logged {
                        log(&format!(
                            "link: dropped a {full_len} byte SDU, longer than the {RECV_BUFFER_LEN} byte buffer; later ones are dropped unlogged"
                        ));
                        self.truncation_logged = true;
                    }
                    // Pods that send nothing but oversized SDUs must not keep the runtime in this loop.
                    task::yield_now().await;
                }
                Ok(Received::TimedOut) => ready.clear_ready(),
                Ok(Received::Closed) => return None,
                Err(error) => {
                    log(&format!("link: {error}"));
                    return None;
                }
            }
        }
    }
}
