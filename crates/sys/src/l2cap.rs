//! An L2CAP sequenced packet channel: one `recv` is one SDU.

use std::os::fd::OwnedFd;
use std::time::Duration;

use pods_proto::addr::BdAddr;
use rustix::io::Errno;
use rustix::net::addr::{SocketAddrArg, SocketAddrLen, SocketAddrOpaque};
use rustix::net::sockopt::{Timeout, set_socket_timeout};
use rustix::net::{AddressFamily, RecvFlags, SendFlags, SocketFlags, SocketType, socket_with};

/// The size of `struct sockaddr_l2` in the kernel's include/net/bluetooth/l2cap.h.
const SOCKADDR_L2_LEN: usize = 14;
/// That size as the length the kernel is told; `TryFrom` is not const, so a test holds the two equal.
const SOCKADDR_L2_SOCKLEN: SocketAddrLen = 14;
/// `l2_cid` is zero for a channel that is reached through a PSM.
const NO_FIXED_CHANNEL: u8 = 0x00;
/// `l2_bdaddr_type` for a classic link, `BDADDR_BREDR` in include/net/bluetooth/bluetooth.h.
const BDADDR_BREDR: u8 = 0x00;
/// The byte the compiler adds after `l2_bdaddr_type` to keep the struct two byte aligned.
const PADDING: u8 = 0x00;

/// A connected sequenced packet socket; closing it is dropping it.
#[derive(Debug)]
pub struct Channel {
    fd: OwnedFd,
}

/// What one `recv` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    /// One whole SDU of this many bytes is in the buffer.
    Sdu(usize),
    /// The SDU was `full_len` bytes; the buffer holds its start and the rest is gone.
    Truncated { full_len: usize },
    /// The receive timeout passed with nothing to read.
    TimedOut,
    /// A read of zero bytes: the peer closed the channel, and an empty SDU would look the same.
    Closed,
}

/// The step that failed and the kernel's reason; never the peer address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChannelError {
    /// The kernel would not create the socket, as when it has no Bluetooth support.
    #[error("could not open the L2CAP socket: {0}")]
    Socket(Errno),
    /// The peer did not answer or refused; a connect that outlasts the timeout is `EINPROGRESS` (`bt_sock_wait_state` in `net/bluetooth/af_bluetooth.c`).
    #[error("could not connect the L2CAP channel: {0}")]
    Connect(Errno),
    /// The send or the receive timeout could not be set.
    #[error("could not set a timeout on the L2CAP socket: {0}")]
    Option(Errno),
    /// Any receive failure other than the timeout, a signal included.
    #[error("could not receive from the L2CAP channel: {0}")]
    Recv(Errno),
    /// The kernel took none of the SDU.
    #[error("could not send on the L2CAP channel: {0}")]
    Send(Errno),
    /// The kernel took part of the SDU, which a sequenced packet socket must never do.
    #[error("could not send on the L2CAP channel: only {sent} of {len} bytes went out")]
    ShortSend { sent: usize, len: usize },
}

/// `struct sockaddr_l2` as bytes, so that no padding byte is left unwritten.
struct SockaddrL2([u8; SOCKADDR_L2_LEN]);

impl SockaddrL2 {
    // Sample output for 11:22:33:44:55:66 and PSM 0x1001: 1f 00 01 10 66 55 44 33 22 11 00 00 00 00.
    #[expect(
        clippy::host_endian_bytes,
        reason = "the kernel reads sa_family_t in host order; the PSM is little endian on every host"
    )]
    fn new(peer: BdAddr, psm: u16) -> Self {
        let [family_0, family_1] = AddressFamily::BLUETOOTH.as_raw().to_ne_bytes();
        let [psm_0, psm_1] = psm.to_le_bytes();
        let [a0, a1, a2, a3, a4, a5] = peer.octets();
        Self([
            family_0,
            family_1,
            psm_0,
            psm_1,
            a5,
            a4,
            a3,
            a2,
            a1,
            a0,
            NO_FIXED_CHANNEL,
            NO_FIXED_CHANNEL,
            BDADDR_BREDR,
            PADDING,
        ])
    }
}

#[expect(
    unsafe_code,
    reason = "rustix has no L2CAP address type, so this crate supplies the one socket address it lacks"
)]
// SAFETY: `f` gets the start of `self.0`, initialised bytes borrowed for the whole call, and `SOCKADDR_L2_SOCKLEN`, which `sockaddr_bytes_are_the_kernel_layout` holds equal to their count; the bytes are a `struct sockaddr_l2`, read and never written.
unsafe impl SocketAddrArg for SockaddrL2 {
    unsafe fn with_sockaddr<R>(
        &self,
        f: impl FnOnce(*const SocketAddrOpaque, SocketAddrLen) -> R,
    ) -> R {
        f(self.0.as_ptr().cast(), SOCKADDR_L2_SOCKLEN)
    }
}

/// Opens a channel to `psm` on `peer`; `timeout` bounds the connect and every later send.
pub fn connect(peer: BdAddr, psm: u16, timeout: Duration) -> Result<Channel, ChannelError> {
    // Protocol `None` is 0, which is `BTPROTO_L2CAP` in include/net/bluetooth/bluetooth.h.
    let fd = socket_with(
        AddressFamily::BLUETOOTH,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .map_err(ChannelError::Socket)?;
    // A blocking L2CAP connect waits no longer than the send timeout (l2cap_sock_connect in net/bluetooth/l2cap_sock.c).
    set_socket_timeout(&fd, Timeout::Send, Some(timeout)).map_err(ChannelError::Option)?;
    rustix::net::connect(&fd, &SockaddrL2::new(peer, psm)).map_err(ChannelError::Connect)?;
    Ok(Channel { fd })
}

/// Wraps any connected `SOCK_SEQPACKET` socket.
#[must_use]
pub fn from_fd(fd: OwnedFd) -> Channel {
    Channel { fd }
}

impl Channel {
    /// Makes `recv` return `TimedOut` after waiting this long; zero is refused.
    pub fn set_recv_timeout(&self, timeout: Duration) -> Result<(), ChannelError> {
        set_socket_timeout(&self.fd, Timeout::Recv, Some(timeout)).map_err(ChannelError::Option)
    }

    /// Reads one SDU into `buf`; with a receive timeout set, a signal ends the wait as `Err(ChannelError::Recv(Errno::INTR))` and is never retried here, so the caller can check its stop flag (with none set, the kernel restarts the read for a handler installed with `SA_RESTART`).
    pub fn recv(&self, buf: &mut [u8]) -> Result<Received, ChannelError> {
        // `TRUNC` makes the kernel return the length of the whole SDU, not of the part that fitted.
        match rustix::net::recv(&self.fd, buf, RecvFlags::TRUNC) {
            Ok((_, 0)) => Ok(Received::Closed),
            Ok((copied, full_len)) if full_len > copied => Ok(Received::Truncated { full_len }),
            Ok((copied, _)) => Ok(Received::Sdu(copied)),
            Err(Errno::AGAIN) => Ok(Received::TimedOut),
            Err(errno) => Err(ChannelError::Recv(errno)),
        }
    }

    /// Writes one SDU.
    pub fn send(&self, sdu: &[u8]) -> Result<(), ChannelError> {
        let len = sdu.len();
        let sent =
            rustix::net::send(&self.fd, sdu, SendFlags::empty()).map_err(ChannelError::Send)?;
        if sent == len {
            Ok(())
        } else {
            Err(ChannelError::ShortSend { sent, len })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use rustix::net::socketpair;

    use super::*;

    const BUFFER_LEN: usize = 4096;
    const RECV_TIMEOUT: Duration = Duration::from_millis(10);

    fn channel_pair() -> (Channel, Channel) {
        let (near, far) = socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        (from_fd(near), from_fd(far))
    }

    #[test]
    fn sockaddr_bytes_are_the_kernel_layout() {
        let peer = "11:22:33:44:55:66".parse().unwrap();
        let addr = SockaddrL2::new(peer, 0x1001);
        // The family reads `1f 00` on a little endian host, which x86-64 and aarch64 Linux both are.
        assert_eq!(
            addr.0,
            [
                0x1f, 0x00, 0x01, 0x10, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00, 0x00, 0x00, 0x00
            ]
        );
        // Checked before `as_any`, which copies that many bytes out of the array.
        assert_eq!(usize::try_from(SOCKADDR_L2_SOCKLEN), Ok(addr.0.len()));
        let any = addr.as_any();
        assert_eq!(any.addr_len(), SOCKADDR_L2_SOCKLEN);
        assert_eq!(any.address_family(), AddressFamily::BLUETOOTH);
    }

    #[test]
    fn message_boundaries_are_kept() {
        let (near, far) = channel_pair();
        let sizes = [1, 338, BUFFER_LEN];
        for size in sizes {
            near.send(&vec![0xa5; size]).unwrap();
        }
        let mut buf = [0_u8; BUFFER_LEN];
        for size in sizes {
            assert_eq!(far.recv(&mut buf).unwrap(), Received::Sdu(size));
            assert!(buf[..size].iter().all(|byte| *byte == 0xa5));
        }
    }

    #[test]
    fn a_message_longer_than_the_buffer_is_reported_truncated() {
        let (near, far) = channel_pair();
        near.send(&[0; 5000]).unwrap();
        let mut buf = [0_u8; BUFFER_LEN];
        assert_eq!(
            far.recv(&mut buf).unwrap(),
            Received::Truncated { full_len: 5000 }
        );
    }

    #[test]
    fn recv_times_out() {
        let (_near, far) = channel_pair();
        far.set_recv_timeout(RECV_TIMEOUT).unwrap();
        let mut buf = [0_u8; BUFFER_LEN];
        let before = Instant::now();
        assert_eq!(far.recv(&mut buf).unwrap(), Received::TimedOut);
        assert!(before.elapsed() >= RECV_TIMEOUT);
    }

    #[test]
    fn a_zero_timeout_is_an_error_naming_the_timeout() {
        let (_near, far) = channel_pair();
        let error = far.set_recv_timeout(Duration::ZERO).unwrap_err();
        assert_eq!(error, ChannelError::Option(Errno::INVAL));
        assert!(error.to_string().contains("timeout"), "{error}");
    }

    #[test]
    fn peer_close_is_closed_not_an_error() {
        let (near, far) = channel_pair();
        drop(near);
        let mut buf = [0_u8; BUFFER_LEN];
        assert_eq!(far.recv(&mut buf).unwrap(), Received::Closed);
    }

    #[test]
    fn send_to_a_closed_peer_is_an_error_naming_send() {
        let (near, far) = channel_pair();
        drop(far);
        let error = near.send(&[0x04]).unwrap_err();
        assert!(matches!(error, ChannelError::Send(_)), "{error:?}");
        assert!(error.to_string().contains("send"), "{error}");
    }

    #[test]
    fn connect_without_bluetooth_is_an_error_not_a_panic() {
        let peer = "02:00:00:00:00:01".parse().unwrap();
        let error = connect(peer, 0x1001, Duration::from_secs(1)).unwrap_err();
        // A host that cannot open a Bluetooth socket must fail at `Socket`, and one that can must fail at `Connect`.
        match socket_with(
            AddressFamily::BLUETOOTH,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        ) {
            Ok(_) => assert!(matches!(error, ChannelError::Connect(_)), "{error:?}"),
            Err(errno) => assert_eq!(error, ChannelError::Socket(errno)),
        }
        assert!(!error.to_string().contains("02:00"), "{error}");
        assert!(!format!("{error:?}").contains("02:00"), "{error:?}");
    }

    #[test]
    fn every_error_names_its_step() {
        let errno = Errno::PIPE;
        for (error, step) in [
            (
                ChannelError::Socket(errno),
                "could not open the L2CAP socket",
            ),
            (
                ChannelError::Connect(errno),
                "could not connect the L2CAP channel",
            ),
            (
                ChannelError::Option(errno),
                "could not set a timeout on the L2CAP socket",
            ),
            (
                ChannelError::Recv(errno),
                "could not receive from the L2CAP channel",
            ),
            (
                ChannelError::Send(errno),
                "could not send on the L2CAP channel",
            ),
        ] {
            assert_eq!(error.to_string(), format!("{step}: {errno}"));
        }
        assert_eq!(
            ChannelError::ShortSend { sent: 3, len: 19 }.to_string(),
            "could not send on the L2CAP channel: only 3 of 19 bytes went out"
        );
    }
}
