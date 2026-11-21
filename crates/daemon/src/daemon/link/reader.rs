//! The reader thread: it connects, forwards each SDU to the driver, and reports how the channel ended, exactly once.

use std::sync::Arc;

use pods_core::link::End;
use pods_proto::addr::BdAddr;
use pods_sys::l2cap::{Channel, Received};
use tokio::sync::mpsc;

use super::{Connector, Input};
use crate::daemon::log;

/// The receive buffer of aacp.rs in librepods-org/librepods pull request 655; a longer SDU is dropped.
pub(super) const RECV_BUFFER_LEN: usize = 4096;

/// The thread's body; it never touches the state, and its last message is its one `Ended`.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the thread's body owns what its spawn moved in"
)]
pub(super) fn run(connector: Connector, device: BdAddr, feedback: mpsc::Sender<Input>) {
    let end = match connector(device) {
        Ok(channel) => {
            forward(&Arc::new(channel), &feedback);
            End::Closed
        }
        Err(error) => {
            log(&format!("link: {error}"));
            End::ConnectFailed
        }
    };
    // A driver that is gone has stopped and needs no end.
    feedback.blocking_send(Input::Ended(end)).ok();
}

/// Sends `Opened`, then every SDU until the channel closes, fails or the driver is gone.
fn forward(channel: &Arc<Channel>, feedback: &mpsc::Sender<Input>) {
    if feedback
        .blocking_send(Input::Opened(Arc::clone(channel)))
        .is_err()
    {
        return;
    }
    let mut buf = vec![0_u8; RECV_BUFFER_LEN];
    let mut truncation_logged = false;
    loop {
        match channel.recv(&mut buf) {
            Ok(Received::Sdu(len)) => {
                let sdu = buf.get(..len).unwrap_or_default();
                if feedback.blocking_send(Input::Sdu(sdu.into())).is_err() {
                    return;
                }
            }
            Ok(Received::Truncated { full_len }) => {
                if !truncation_logged {
                    log(&format!(
                        "link: dropped a {full_len} byte SDU, longer than the {RECV_BUFFER_LEN} byte buffer; later ones are dropped unlogged"
                    ));
                    truncation_logged = true;
                }
            }
            // No receive timeout is set on the channel, so this never comes.
            Ok(Received::TimedOut) => {}
            Ok(Received::Closed) => return,
            Err(error) => {
                log(&format!("link: {error}"));
                return;
            }
        }
    }
}
