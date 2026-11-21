//! An L2CAP sequenced packet channel: one `recv` is one SDU.

use std::os::fd::OwnedFd;
use std::time::Duration;

use pods_proto::addr::BdAddr;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use rustix::net::addr::{SocketAddrArg, SocketAddrLen, SocketAddrOpaque};
use rustix::net::sockopt::{Timeout, set_socket_timeout};
use rustix::net::{
    AddressFamily, RecvFlags, SendFlags, Shutdown, SocketFlags, SocketType, socket_with,
};

/// The size of `struct sockaddr_l2` in the kernel's include/net/bluetooth/l2cap.h.
const SOCKADDR_L2_LEN: usize = 14;
/// `TryFrom` is not const, so a test holds this equal to `SOCKADDR_L2_LEN`.
const SOCKADDR_L2_SOCKLEN: SocketAddrLen = 14;
/// `l2_cid` is zero for a channel that is reached through a PSM.
const NO_FIXED_CHANNEL: u8 = 0x00;
/// `l2_bdaddr_type` for a classic link, `BDADDR_BREDR` in include/net/bluetooth/bluetooth.h.
const BDADDR_BREDR: u8 = 0x00;
/// The byte the compiler adds after `l2_bdaddr_type` to keep the struct two byte aligned.
const PADDING: u8 = 0x00;

#[derive(Debug)]
pub struct Channel {
    fd: OwnedFd,
}

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
    #[error("could not open the L2CAP socket: {0}")]
    Socket(Errno),
    /// The peer did not answer or refused; a connect that outlasts the timeout is `EINPROGRESS` (`bt_sock_wait_state` in `net/bluetooth/af_bluetooth.c`).
    #[error("could not connect the L2CAP channel: {0}")]
    Connect(Errno),
    #[error("could not set a timeout on the L2CAP socket: {0}")]
    Option(Errno),
    #[error("could not receive from the L2CAP channel: {0}")]
    Recv(Errno),
    #[error("could not send on the L2CAP channel: {0}")]
    Send(Errno),
    /// The kernel took part of the SDU, which a sequenced packet socket must never do.
    #[error("could not send on the L2CAP channel: only {sent} of {len} bytes went out")]
    ShortSend { sent: usize, len: usize },
    #[error("could not wait on the L2CAP channels: {0}")]
    Wait(Errno),
    #[error("could not shut down the L2CAP channel: {0}")]
    Shutdown(Errno),
}

/// `struct sockaddr_l2` as bytes, so that no padding byte is left unwritten.
struct SockaddrL2([u8; SOCKADDR_L2_LEN]);

impl SockaddrL2 {
    // Sample output for 11:22:33:44:55:66 and PSM 0x1001: 1f 00 01 10 66 55 44 33 22 11 00 00 00 00
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

/// Opens the channel `psm` names on `peer`; `timeout` bounds the connect and every later send.
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
    // No test sees which PSM this dials; only a connect to a second PSM on hardware proves it.
    rustix::net::connect(&fd, &SockaddrL2::new(peer, psm)).map_err(ChannelError::Connect)?;
    Ok(Channel { fd })
}

/// Waits until one of `channels` has something to read or has hung up and returns its index, the lowest when several do; `None` is `timeout` passing first. A signal that runs a handler ends the wait as `Err(ChannelError::Wait(Errno::INTR))`, whatever its restart flag, and is never retried here, so the caller can check its stop flag.
pub fn wait_any(channels: &[&Channel], timeout: Duration) -> Result<Option<usize>, ChannelError> {
    // A wait longer than the kernel's seconds field holds is refused with the errno the kernel gives a bad timeout.
    let Ok(timeout) = Timespec::try_from(timeout) else {
        return Err(ChannelError::Wait(Errno::INVAL));
    };
    let mut fds: Vec<PollFd<'_>> = channels
        .iter()
        .map(|channel| PollFd::new(&channel.fd, PollFlags::IN))
        .collect();
    poll(&mut fds, Some(&timeout)).map_err(ChannelError::Wait)?;
    // The kernel reports a hang up or an error whether asked or not, and `recv` then says which it was.
    Ok(fds.iter().position(|fd| !fd.revents().is_empty()))
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

    pub fn send(&self, sdu: &[u8]) -> Result<(), ChannelError> {
        self.send_with(sdu, SendFlags::empty())
    }

    /// Writes one SDU without waiting; a full send buffer is `Err(ChannelError::Send(Errno::AGAIN))`.
    pub fn try_send(&self, sdu: &[u8]) -> Result<(), ChannelError> {
        self.send_with(sdu, SendFlags::DONTWAIT)
    }

    /// Shuts down both directions so a `recv` blocked in another thread returns `Closed`, proven on an `AF_UNIX` socket pair and not yet on an L2CAP socket; an error from a channel already down is expected.
    pub fn shutdown(&self) -> Result<(), ChannelError> {
        rustix::net::shutdown(&self.fd, Shutdown::Both).map_err(ChannelError::Shutdown)
    }

    fn send_with(&self, sdu: &[u8], flags: SendFlags) -> Result<(), ChannelError> {
        let len = sdu.len();
        let sent = rustix::net::send(&self.fd, sdu, flags).map_err(ChannelError::Send)?;
        if sent == len {
            Ok(())
        } else {
            Err(ChannelError::ShortSend { sent, len })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Instant;

    use pods_proto::att::{self, AttPdu, HandleInfo, Uuid};
    use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
    use rustix::net::socketpair;

    use super::*;

    const BUFFER_LEN: usize = 4096;
    /// Any size strictly between one byte and `BUFFER_LEN`, so the middle SDU is at neither edge.
    const MIDDLE_SDU_LEN: usize = 338;
    const RECV_TIMEOUT: Duration = Duration::from_millis(10);
    /// Longer than any loaded host takes to hand over a packet that is already queued.
    const READY_WAIT: Duration = Duration::from_secs(5);
    /// The range that reads a whole attribute table.
    const FIRST_HANDLE: u16 = 0x0001;
    const LAST_HANDLE: u16 = 0xffff;
    /// The bound on a send that must not wait; a waiting send takes `SEND_TIMEOUT`.
    const AT_ONCE: Duration = Duration::from_secs(1);
    /// Longer than `AT_ONCE`, so a waiting send fails the time check instead of hanging.
    const SEND_TIMEOUT: Duration = Duration::from_secs(2);
    /// Far more one byte SDUs than a socket pair's send buffer holds.
    const FILL_LIMIT: usize = 100_000;

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
        // The family reads `1f 00` on a little endian host: x86-64 and aarch64 Linux both are.
        for (psm, bytes) in [
            (
                CONTROL_PSM,
                [
                    0x1f, 0x00, 0x01, 0x10, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00, 0x00, 0x00,
                    0x00,
                ],
            ),
            (
                ATT_PSM,
                [
                    0x1f, 0x00, 0x1f, 0x00, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00, 0x00, 0x00,
                    0x00,
                ],
            ),
        ] {
            let addr = SockaddrL2::new(peer, psm);
            assert_eq!(addr.0, bytes, "PSM {psm:#06x}");
            // Checked before `as_any`, which copies that many bytes out of the array.
            assert_eq!(usize::try_from(SOCKADDR_L2_SOCKLEN), Ok(addr.0.len()));
            let any = addr.as_any();
            assert_eq!(any.addr_len(), SOCKADDR_L2_SOCKLEN);
            assert_eq!(any.address_family(), AddressFamily::BLUETOOTH);
        }
    }

    #[test]
    fn message_boundaries_are_kept() {
        let (near, far) = channel_pair();
        let sizes = [1, MIDDLE_SDU_LEN, BUFFER_LEN];
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
    fn wait_any_names_the_channel_that_has_data() {
        let (first_near, first_far) = channel_pair();
        let (second_near, second_far) = channel_pair();
        let watched = [&first_far, &second_far];
        second_near.send(&[0x1b]).unwrap();
        assert_eq!(wait_any(&watched, READY_WAIT), Ok(Some(1)));
        let mut buf = [0_u8; BUFFER_LEN];
        assert_eq!(second_far.recv(&mut buf).unwrap(), Received::Sdu(1));
        first_near.send(&[0x1b]).unwrap();
        assert_eq!(wait_any(&watched, READY_WAIT), Ok(Some(0)));
    }

    #[test]
    fn wait_any_times_out_with_none() {
        let (_first_near, first_far) = channel_pair();
        let (_second_near, second_far) = channel_pair();
        let before = Instant::now();
        assert_eq!(wait_any(&[&first_far, &second_far], RECV_TIMEOUT), Ok(None));
        assert!(before.elapsed() >= RECV_TIMEOUT);
        assert_eq!(wait_any(&[], RECV_TIMEOUT), Ok(None));
    }

    #[test]
    fn wait_any_reports_a_hung_up_channel_as_ready() {
        let (_first_near, first_far) = channel_pair();
        let (second_near, second_far) = channel_pair();
        drop(second_near);
        assert_eq!(
            wait_any(&[&first_far, &second_far], READY_WAIT),
            Ok(Some(1))
        );
        let mut buf = [0_u8; BUFFER_LEN];
        assert_eq!(second_far.recv(&mut buf).unwrap(), Received::Closed);
    }

    #[test]
    fn wait_any_reports_an_error_with_nothing_to_read_as_ready() {
        let (_near, far) = channel_pair();
        // A socket pair raises IN with every hang up, so a pipe's write end with no reader stands in for a channel that raises ERR alone.
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let broken = from_fd(OwnedFd::from(writer));
        assert_eq!(wait_any(&[&far, &broken], READY_WAIT), Ok(Some(1)));
    }

    #[test]
    fn wait_any_with_both_ready_names_the_first() {
        let (first_near, first_far) = channel_pair();
        let (second_near, second_far) = channel_pair();
        second_near.send(&[0x1b]).unwrap();
        first_near.send(&[0x1b]).unwrap();
        assert_eq!(
            wait_any(&[&first_far, &second_far], READY_WAIT),
            Ok(Some(0))
        );
        assert_eq!(
            wait_any(&[&second_far, &first_far], READY_WAIT),
            Ok(Some(0))
        );
    }

    #[test]
    fn wait_any_refuses_a_timeout_longer_than_the_kernel_can_hold() {
        let (_near, far) = channel_pair();
        let error = wait_any(&[&far], Duration::MAX).unwrap_err();
        assert_eq!(error, ChannelError::Wait(Errno::INVAL));
        assert!(error.to_string().contains("wait"), "{error}");
    }

    /// Receives one SDU, which the test has already seen arrive or sent itself.
    fn recv_sdu<'a>(channel: &Channel, buf: &'a mut [u8; BUFFER_LEN]) -> &'a [u8] {
        let received = channel.recv(buf).unwrap();
        let Received::Sdu(len) = received else {
            panic!("not a whole SDU: {received:?}");
        };
        &buf[..len]
    }

    #[test]
    fn a_find_information_exchange_reads_back_across_a_socketpair() {
        let (host, pods) = channel_pair();
        let (idle_host, _idle_pods) = channel_pair();
        let watched = [&idle_host, &host];
        let mut buf = [0_u8; BUFFER_LEN];

        host.send(&att::find_information_request(FIRST_HANDLE, LAST_HANDLE))
            .unwrap();
        let request = recv_sdu(&pods, &mut buf);
        assert_eq!(request, [0x04, 0x01, 0x00, 0xff, 0xff]);
        assert_eq!(AttPdu::parse(request), Ok(AttPdu::Request { opcode: 0x04 }));
        pods.send(&[0x05, 0x01, 0x01, 0x00, 0x00, 0x28, 0x02, 0x00, 0x03, 0x28])
            .unwrap();
        assert_eq!(wait_any(&watched, READY_WAIT), Ok(Some(1)));
        let reply = recv_sdu(&host, &mut buf);
        let Ok(AttPdu::Information(pairs)) = AttPdu::parse(reply) else {
            panic!("not a Find Information reply: {reply:02x?}");
        };
        let listed: Vec<_> = pairs.collect();
        let expected =
            [(0x0001, 0x2800), (0x0002, att::CHARACTERISTIC_DECLARATION)].map(|(handle, uuid)| {
                HandleInfo {
                    handle,
                    uuid: Uuid::Short(uuid),
                }
            });
        assert_eq!(listed, expected);

        let next = listed[1].handle + 1;
        host.send(&att::find_information_request(next, LAST_HANDLE))
            .unwrap();
        let [opcode, ..] = *recv_sdu(&pods, &mut buf) else {
            panic!("the second request arrived empty");
        };
        pods.send(&att::error_response(opcode, next, att::ATTRIBUTE_NOT_FOUND))
            .unwrap();
        assert_eq!(wait_any(&watched, READY_WAIT), Ok(Some(1)));
        let end_of_table = AttPdu::Error {
            request: 0x04,
            handle: 0x0003,
            code: att::ATTRIBUTE_NOT_FOUND,
        };
        assert_eq!(AttPdu::parse(recv_sdu(&host, &mut buf)), Ok(end_of_table));
    }

    /// Polls `/proc` until thread `task` sleeps, which for a reader means blocked in `recv`.
    fn sleeps_within(task: &Path, wait: Duration) -> Result<bool, String> {
        let stat = Path::new("/proc").join(task).join("stat");
        let started = Instant::now();
        while started.elapsed() < wait {
            // Sample input: "1317937 (pods_sys-1a2b3c) S 1317935 1317936 ..."; the name may hold ") ", so the state follows the last one.
            let line = std::fs::read_to_string(&stat)
                .map_err(|error| format!("cannot read {}: {error}", stat.display()))?;
            if line
                .rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('S'))
            {
                return Ok(true);
            }
            std::thread::yield_now();
        }
        Ok(false)
    }

    #[test]
    fn shutdown_wakes_a_recv_blocked_in_another_thread() {
        let (near, far) = channel_pair();
        let (task_tx, task_rx) = mpsc::channel();
        let (woke_tx, woke_rx) = mpsc::channel();
        let (blocked, shut, woke) = std::thread::scope(|scope| {
            scope.spawn(|| {
                task_tx
                    .send(std::fs::read_link("/proc/thread-self"))
                    .unwrap();
                woke_tx.send(far.recv(&mut [0_u8; BUFFER_LEN])).unwrap();
            });
            let blocked = match task_rx.recv() {
                Ok(Ok(task)) => sleeps_within(&task, READY_WAIT),
                Ok(Err(error)) => Err(format!("cannot read /proc/thread-self: {error}")),
                Err(error) => Err(format!("the reader sent no task path: {error}")),
            };
            let shut = far.shutdown();
            let woke = woke_rx.recv_timeout(READY_WAIT);
            // A reader still blocked would hang the scope, so drop the peer before any assert.
            drop(near);
            (blocked, shut, woke)
        });
        assert_eq!(blocked, Ok(true), "the reader never blocked in recv");
        assert_eq!(shut, Ok(()));
        assert_eq!(woke, Ok(Ok(Received::Closed)));
    }

    #[test]
    fn shutdown_closes_both_directions() {
        let (near, far) = channel_pair();
        near.set_recv_timeout(RECV_TIMEOUT).unwrap();
        far.shutdown().unwrap();
        let mut buf = [0_u8; BUFFER_LEN];
        assert_eq!(near.recv(&mut buf).unwrap(), Received::Closed);
        assert_eq!(near.send(&[0x04]), Err(ChannelError::Send(Errno::PIPE)));
    }

    #[test]
    fn shutdown_twice_is_not_a_panic() {
        let (_near, far) = channel_pair();
        assert_eq!(far.shutdown(), Ok(()));
        assert_eq!(far.shutdown(), Ok(()));
    }

    #[test]
    fn shutdown_of_a_non_socket_is_an_error_naming_shutdown() {
        let (reader, _writer) = std::io::pipe().unwrap();
        let error = from_fd(OwnedFd::from(reader)).shutdown().unwrap_err();
        assert_eq!(error, ChannelError::Shutdown(Errno::NOTSOCK));
        assert!(error.to_string().contains("shut down"), "{error}");
    }

    #[test]
    fn try_send_on_a_full_buffer_is_again_and_returns_at_once() {
        let (near, _far) = channel_pair();
        set_socket_timeout(&near.fd, Timeout::Send, Some(SEND_TIMEOUT)).unwrap();
        let mut last = None;
        for _ in 0..FILL_LIMIT {
            let before = Instant::now();
            let sent = near.try_send(&[0x5a]);
            if let Err(error) = sent {
                last = Some((error, before.elapsed()));
                break;
            }
        }
        let (error, took) = last.expect("the send buffer never filled");
        assert_eq!(error, ChannelError::Send(Errno::AGAIN));
        assert!(took < AT_ONCE, "the full buffer took {took:?}");
    }

    #[test]
    fn try_send_keeps_message_boundaries() {
        let (near, far) = channel_pair();
        let sizes = [1, MIDDLE_SDU_LEN, BUFFER_LEN];
        for (fill, size) in [0x11_u8, 0x22, 0x33].into_iter().zip(sizes) {
            near.try_send(&vec![fill; size]).unwrap();
        }
        let mut buf = [0_u8; BUFFER_LEN];
        for (fill, size) in [0x11_u8, 0x22, 0x33].into_iter().zip(sizes) {
            assert_eq!(far.recv(&mut buf).unwrap(), Received::Sdu(size));
            assert!(buf[..size].iter().all(|byte| *byte == fill));
        }
    }

    #[test]
    fn connect_without_bluetooth_is_an_error_not_a_panic() {
        let peer = "02:00:00:00:00:01".parse().unwrap();
        let error = connect(peer, CONTROL_PSM, Duration::from_secs(1)).unwrap_err();
        // A host with no Bluetooth socket fails at `Socket`; one with it fails at `Connect`.
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
            (
                ChannelError::Wait(errno),
                "could not wait on the L2CAP channels",
            ),
            (
                ChannelError::Shutdown(errno),
                "could not shut down the L2CAP channel",
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
