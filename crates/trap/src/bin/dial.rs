//! The trap's stand-in child: makes one kind of call and prints one line per result.

use std::fmt::Write;
use std::io::{self, BufRead};
use std::num::NonZeroU32;
use std::os::fd::{AsRawFd, OwnedFd};
use std::process::ExitCode;
use std::time::Duration;

use pods_proto::addr::BdAddr;
use pods_sys::l2cap::{self, ChannelError, Received};
use rustix::event::{Timespec, poll};
use rustix::io::Errno;
use rustix::net::{
    AddressFamily, Protocol, RecvFlags, SendFlags, SocketAddrUnix, SocketFlags, SocketType,
    socket_with,
};

/// The stand-in ends by itself after this many seconds, whatever happens.
const LIFETIME_SECONDS: u32 = 10;
/// Bounds the connect, the send and the read of the reply.
const DIAL_TIMEOUT: Duration = Duration::from_secs(3);
const IDLE: Timespec = Timespec {
    tv_sec: 2,
    tv_nsec: 0,
};
const REPLY_LEN: usize = 4096;
/// `BTPROTO_HCI` in include/net/bluetooth/bluetooth.h, a protocol the trap refuses.
const BTPROTO_HCI: u32 = 1;
/// The legacy L2CAP socket option level in include/net/bluetooth/bluetooth.h.
const SOL_L2CAP: libc::c_int = 6;
const L2CAP_OPTIONS: libc::c_int = 0x01;
/// An L2CAP option in include/net/bluetooth/l2cap.h that the trap does not serve.
const L2CAP_CONNINFO: libc::c_int = 0x02;
/// Kernel defaults: outgoing and incoming MTU 672, flush timeout ffff, Basic mode, FCS 1, max transmit 3, padding, window 63.
const DEFAULT_L2CAP_OPTIONS: [u8; 12] = [0xa0, 0x02, 0xa0, 0x02, 0xff, 0xff, 0, 1, 3, 0, 63, 0];
const SHORT_OPTIONS_LEN: usize = 4;

fn main() -> ExitCode {
    #[expect(
        unsafe_code,
        reason = "alarm has no safe binding in the crates this workspace uses"
    )]
    // SAFETY: `alarm` takes a number and only sets this process's timer; `SIGALRM` keeps its default action, which ends the process.
    unsafe {
        libc::alarm(LIFETIME_SECONDS);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(mode) = Mode::parse(&args) else {
        report_usage();
        return ExitCode::from(2);
    };
    match mode {
        Mode::Dial { device, psms } => dial_each(device, &psms),
        Mode::DialWithImtu { device, psm, imtu } => say(&dial(device, psm, Some(imtu))),
        Mode::Late { device, psm, imtu } => late(device, psm, imtu),
        Mode::Socket => say(&socket()),
        Mode::Raw => say(&raw()),
        Mode::OtherOption => say(&other_option()),
        Mode::ReusedTcpSocket => say(&reused_tcp_socket()),
        Mode::OptionAfterConnect { device, psm } => say(&option_after_connect(device, psm)),
        Mode::ShortOption => say(&short_option()),
        Mode::Unix { name } => say(&unix(&name)),
        Mode::Idle => say(&idle()),
    }
    ExitCode::SUCCESS
}

enum Mode {
    Dial {
        device: BdAddr,
        psms: Vec<u16>,
    },
    DialWithImtu {
        device: BdAddr,
        psm: u16,
        imtu: u16,
    },
    Late {
        device: BdAddr,
        psm: u16,
        imtu: Option<u16>,
    },
    Socket,
    Raw,
    OtherOption,
    ReusedTcpSocket,
    OptionAfterConnect {
        device: BdAddr,
        psm: u16,
    },
    ShortOption,
    Unix {
        name: String,
    },
    Idle,
}

impl Mode {
    // Sample input: `dial 02:00:00:00:00:01 0x1001`.
    fn parse(args: &[&str]) -> Option<Self> {
        Some(match *args {
            ["dial", device, psm] => Self::Dial {
                device: device.parse().ok()?,
                psms: vec![parse_psm(psm)?],
            },
            ["dial-imtu", device, psm, imtu] => Self::DialWithImtu {
                device: device.parse().ok()?,
                psm: parse_psm(psm)?,
                imtu: imtu.parse().ok()?,
            },
            ["twice", device, first, second] => Self::Dial {
                device: device.parse().ok()?,
                psms: vec![parse_psm(first)?, parse_psm(second)?],
            },
            ["late", device, psm] => Self::Late {
                device: device.parse().ok()?,
                psm: parse_psm(psm)?,
                imtu: None,
            },
            ["late", device, psm, imtu] => Self::Late {
                device: device.parse().ok()?,
                psm: parse_psm(psm)?,
                imtu: Some(imtu.parse().ok()?),
            },
            ["socket"] => Self::Socket,
            ["raw"] => Self::Raw,
            ["other-option"] => Self::OtherOption,
            ["reused-tcp-socket"] => Self::ReusedTcpSocket,
            ["option-after-connect", device, psm] => Self::OptionAfterConnect {
                device: device.parse().ok()?,
                psm: parse_psm(psm)?,
            },
            ["short-option"] => Self::ShortOption,
            ["unix", name] => Self::Unix {
                name: name.to_owned(),
            },
            ["idle"] => Self::Idle,
            _ => return None,
        })
    }
}

// Sample input: `0x1001`.
fn parse_psm(text: &str) -> Option<u16> {
    u16::from_str_radix(text.strip_prefix("0x")?, 16).ok()
}

/// Prints one line at once, so the test reads each result while the next call waits.
#[expect(
    clippy::print_stdout,
    reason = "the line on standard output is what the test reads"
)]
fn say(line: &str) {
    println!("{line}");
}

#[expect(clippy::print_stderr, reason = "a usage error goes to standard error")]
fn report_usage() {
    eprintln!(
        "usage: trap-dial dial|option-after-connect <address> <psm> | late <address> <psm> [<imtu>] | dial-imtu <address> <psm> <imtu> | twice <address> <psm> <psm> | socket | raw | other-option | reused-tcp-socket | short-option | unix <name> | idle"
    );
}

/// One line per PSM, dialled one after the other.
fn dial_each(device: BdAddr, psms: &[u16]) {
    for psm in psms {
        say(&dial(device, *psm, None));
    }
}

/// Reads one line from standard input first, so the test decides when the dial happens.
fn late(device: BdAddr, psm: u16, imtu: Option<u16>) {
    let mut line = String::new();
    match io::stdin().lock().read_line(&mut line) {
        Ok(_) => say(&dial(device, psm, imtu)),
        Err(error) => say(&format!("error stdin {error}")),
    }
}

fn errno_line(step: &str, errno: Errno) -> String {
    format!("error {step} {}", errno.raw_os_error())
}

/// Connects, sends `ping` and reads one SDU.
fn dial(device: BdAddr, psm: u16, imtu: Option<u16>) -> String {
    let connected = match imtu {
        Some(imtu) => l2cap::connect_with_imtu(device, psm, DIAL_TIMEOUT, imtu),
        None => l2cap::connect(device, psm, DIAL_TIMEOUT),
    };
    let channel = match connected {
        Ok(channel) => channel,
        Err(ChannelError::Socket(errno)) => return errno_line("socket", errno),
        Err(ChannelError::Timeout(errno)) => return errno_line("timeout", errno),
        Err(error @ ChannelError::Option { .. }) => return format!("error option {error}"),
        Err(ChannelError::Connect(errno)) => return errno_line("connect", errno),
        Err(other) => return format!("error connect {other}"),
    };
    if imtu.is_some() {
        match channel.mtus() {
            Ok(mtus) => say(&format!("mtus {} {}", mtus.incoming, mtus.outgoing)),
            Err(error) => return format!("error mtus {error}"),
        }
    }
    let exchanged = channel
        .send(b"ping")
        .and_then(|()| channel.set_recv_timeout(DIAL_TIMEOUT))
        .and_then(|()| {
            let mut reply = [0; REPLY_LEN];
            match channel.recv(&mut reply)? {
                Received::Sdu(len) => Ok(reply.get(..len).map_or_else(String::new, hex)),
                Received::Truncated { .. } => Ok(hex(&reply)),
                Received::Closed => Ok(String::new()),
                Received::TimedOut => Err(ChannelError::Recv(Errno::TIMEDOUT)),
            }
        });
    match exchanged {
        Ok(read) if read.is_empty() => "ok".to_owned(),
        Ok(read) => format!("ok {read}"),
        Err(error) => format!("error exchange {error}"),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _infallible = write!(text, "{byte:02x}");
    }
    text
}

/// Asks for a nonblocking socket; `ok` means a read with nothing sent returned `EAGAIN` at once.
fn socket() -> String {
    let socket = match socket_with(
        AddressFamily::BLUETOOTH,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    ) {
        Ok(socket) => socket,
        Err(errno) => return errno_line("socket", errno),
    };
    let mut byte = [0; 1];
    match rustix::net::recv(&socket, &mut byte, RecvFlags::empty()) {
        Err(Errno::AGAIN) => "ok".to_owned(),
        Err(errno) => errno_line("recv", errno),
        Ok((0, _)) => "error recv end of file, the far end is closed".to_owned(),
        Ok((read, _)) => format!("error recv read {read} bytes nobody sent"),
    }
}

fn bluetooth_socket() -> Result<OwnedFd, Errno> {
    socket_with(
        AddressFamily::BLUETOOTH,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
}

fn other_option() -> String {
    match bluetooth_socket() {
        Ok(socket) => set_l2cap_option(&socket, L2CAP_CONNINFO, &[0]),
        Err(errno) => errno_line("socket", errno),
    }
}

fn reused_tcp_socket() -> String {
    let trapped = match bluetooth_socket() {
        Ok(socket) => socket,
        Err(errno) => return errno_line("socket", errno),
    };
    let number = trapped.as_raw_fd();
    drop(trapped);
    let tcp = match socket_with(
        AddressFamily::INET,
        SocketType::STREAM,
        SocketFlags::CLOEXEC,
        Some(rustix::net::ipproto::TCP),
    ) {
        Ok(socket) => socket,
        Err(errno) => return errno_line("TCP socket", errno),
    };
    if tcp.as_raw_fd() != number {
        return format!("error TCP socket did not reuse descriptor {number}");
    }
    // TCP_NODELAY has the same numeric level and option as L2CAP_OPTIONS.
    match rustix::net::sockopt::set_tcp_nodelay(&tcp, true) {
        Ok(()) => "ok".to_owned(),
        Err(errno) => errno_line("TCP_NODELAY", errno),
    }
}

fn option_after_connect(device: BdAddr, psm: u16) -> String {
    match l2cap::connect(device, psm, DIAL_TIMEOUT) {
        Ok(channel) => set_l2cap_option(&channel, L2CAP_OPTIONS, &DEFAULT_L2CAP_OPTIONS),
        Err(error) => format!("error connect {error}"),
    }
}

fn short_option() -> String {
    let options = DEFAULT_L2CAP_OPTIONS;
    let (short, _) = options.split_at(SHORT_OPTIONS_LEN);
    match bluetooth_socket() {
        Ok(socket) => set_l2cap_option(&socket, L2CAP_OPTIONS, short),
        Err(errno) => errno_line("socket", errno),
    }
}

fn set_l2cap_option(socket: &impl AsRawFd, option: libc::c_int, value: &[u8]) -> String {
    let Ok(len) = libc::socklen_t::try_from(value.len()) else {
        return errno_line("option length", Errno::INVAL);
    };
    #[expect(
        unsafe_code,
        reason = "the stand-in asks for an option rustix does not expose"
    )]
    // SAFETY: the socket is open and the kernel can read len bytes from value for the entire call.
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            SOL_L2CAP,
            option,
            value.as_ptr().cast(),
            len,
        )
    };
    if result == 0 {
        return "ok".to_owned();
    }
    let errno = Errno::from_io_error(&io::Error::last_os_error()).unwrap_or(Errno::IO);
    errno_line("option", errno)
}

fn raw() -> String {
    let protocol = NonZeroU32::new(BTPROTO_HCI).map(Protocol::from_raw);
    match socket_with(
        AddressFamily::BLUETOOTH,
        SocketType::RAW,
        SocketFlags::CLOEXEC,
        protocol,
    ) {
        Ok(_socket) => "ok".to_owned(),
        Err(errno) => errno_line("socket", errno),
    }
}

fn unix(name: &str) -> String {
    let address = match SocketAddrUnix::new_abstract_name(name.as_bytes()) {
        Ok(address) => address,
        Err(errno) => return errno_line("address", errno),
    };
    let socket = match socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    ) {
        Ok(socket) => socket,
        Err(errno) => return errno_line("socket", errno),
    };
    if let Err(errno) = rustix::net::connect(&socket, &address) {
        return errno_line("connect", errno);
    }
    match rustix::net::send(&socket, b"ping", SendFlags::empty()) {
        Ok(_) => "ok".to_owned(),
        Err(errno) => errno_line("send", errno),
    }
}

fn idle() -> String {
    match poll(&mut [], Some(&IDLE)) {
        Ok(_) => "ok".to_owned(),
        Err(errno) => errno_line("poll", errno),
    }
}
