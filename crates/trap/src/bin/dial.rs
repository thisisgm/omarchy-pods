//! The trap's stand-in child: makes one kind of call and prints one line per result.

use std::fmt::Write;
use std::io::{self, BufRead};
use std::num::NonZeroU32;
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
        Mode::Late { device, psm } => late(device, psm),
        Mode::Socket => say(&socket()),
        Mode::Raw => say(&raw()),
        Mode::Unix { name } => say(&unix(&name)),
        Mode::Idle => say(&idle()),
    }
    ExitCode::SUCCESS
}

enum Mode {
    Dial { device: BdAddr, psms: Vec<u16> },
    Late { device: BdAddr, psm: u16 },
    Socket,
    Raw,
    Unix { name: String },
    Idle,
}

impl Mode {
    fn parse(args: &[&str]) -> Option<Self> {
        Some(match *args {
            ["dial", device, psm] => Self::Dial {
                device: device.parse().ok()?,
                psms: vec![parse_psm(psm)?],
            },
            ["twice", device, first, second] => Self::Dial {
                device: device.parse().ok()?,
                psms: vec![parse_psm(first)?, parse_psm(second)?],
            },
            ["late", device, psm] => Self::Late {
                device: device.parse().ok()?,
                psm: parse_psm(psm)?,
            },
            ["socket"] => Self::Socket,
            ["raw"] => Self::Raw,
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
        "usage: trap-dial dial|late <address> <psm> | twice <address> <psm> <psm> | socket | raw | unix <name> | idle"
    );
}

/// One line per PSM, dialled one after the other.
fn dial_each(device: BdAddr, psms: &[u16]) {
    for psm in psms {
        say(&dial(device, *psm));
    }
}

/// Reads one line from standard input first, so the test decides when the dial happens.
fn late(device: BdAddr, psm: u16) {
    let mut line = String::new();
    match io::stdin().lock().read_line(&mut line) {
        Ok(_) => say(&dial(device, psm)),
        Err(error) => say(&format!("error stdin {error}")),
    }
}

fn errno_line(step: &str, errno: Errno) -> String {
    format!("error {step} {}", errno.raw_os_error())
}

/// Connects, sends `ping` and reads one SDU.
fn dial(device: BdAddr, psm: u16) -> String {
    let channel = match l2cap::connect(device, psm, DIAL_TIMEOUT) {
        Ok(channel) => channel,
        Err(ChannelError::Socket(errno)) => return errno_line("socket", errno),
        Err(ChannelError::Option(errno)) => return errno_line("option", errno),
        Err(ChannelError::Connect(errno)) => return errno_line("connect", errno),
        Err(other) => return format!("error connect {other}"),
    };
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
