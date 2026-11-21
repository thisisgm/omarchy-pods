//! The trap's routes, its record of what the child did, and the text each prints.

use std::fmt;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pods_proto::addr::BdAddr;
use pods_sys::l2cap::Channel;
use rustix::io::Errno;

/// Every fixture address starts with these octets, so any other address belongs to a real device.
const TEST_RANGE: [u8; 3] = [0x02, 0x00, 0x00];

pub type Serve = Arc<dyn Fn(Channel) + Send + Sync>;

#[derive(Clone)]
pub enum Answer {
    /// `connect` returns 0, then `serve` gets the far end; it runs on the trap's thread, so it hands the channel on and returns.
    Accept {
        after: Duration,
        serve: Serve,
    },
    Refuse {
        after: Duration,
        errno: Errno,
    },
}

pub struct Route {
    pub device: BdAddr,
    pub psm: u16,
    pub answer: Answer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dial {
    pub at: Instant,
    pub device: BdAddr,
    pub psm: u16,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Accepted,
    Refused(Errno),
    Unrouted,
    /// The caller was gone before the answer reached it.
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    ForeignSocket { kind: u32, protocol: u32 },
    Unrouted { device: BdAddr, psm: u16 },
    BadAddress { cid: u16, address_type: u8 },
    Stranger { fd: u32 },
}

#[derive(Debug, thiserror::Error)]
pub enum TrapError {
    #[error("the kernel offers no seccomp user notification")]
    Unsupported,
    #[error("the child did not start: {0}")]
    Spawn(io::Error),
    #[error("the child's seccomp listener did not arrive within {} ms", crate::HAND_OVER_WAIT.as_millis())]
    HandOver,
    #[error("waited {} ms for {} and {} came{}", waited.as_millis(), plural(*wanted, "dial"), came(dials.len()), DialRecord { dials, spawned: *spawned })]
    Deadline {
        waited: Duration,
        wanted: usize,
        dials: Vec<Dial>,
        spawned: Instant,
    },
    #[error("the child made {} the trap refused:{}{}", plural(violations.len(), "call"), ViolationLines(violations), DialRecord { dials, spawned: *spawned })]
    Violations {
        violations: Vec<Violation>,
        dials: Vec<Dial>,
        spawned: Instant,
    },
    #[error("the trap stopped early: {0}")]
    Supervisor(String),
}

impl PartialEq for TrapError {
    // An `io::Error` has no equality of its own, so two spawn errors are equal when their OS error and kind are.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Unsupported, Self::Unsupported) | (Self::HandOver, Self::HandOver) => true,
            (Self::Spawn(left), Self::Spawn(right)) => {
                left.raw_os_error() == right.raw_os_error() && left.kind() == right.kind()
            }
            (
                Self::Deadline {
                    waited,
                    wanted,
                    dials,
                    spawned,
                },
                Self::Deadline {
                    waited: w,
                    wanted: n,
                    dials: d,
                    spawned: s,
                },
            ) => (waited, wanted, dials, spawned) == (w, n, d, s),
            (
                Self::Violations {
                    violations,
                    dials,
                    spawned,
                },
                Self::Violations {
                    violations: v,
                    dials: d,
                    spawned: s,
                },
            ) => (violations, dials, spawned) == (v, d, s),
            (Self::Supervisor(left), Self::Supervisor(right)) => left == right,
            _ => false,
        }
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn came(count: usize) -> String {
    if count == 0 {
        "none".to_owned()
    } else {
        count.to_string()
    }
}

struct ViolationLines<'a>(&'a [Violation]);

impl fmt::Display for ViolationLines<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .iter()
            .try_for_each(|violation| write!(f, "\n  {violation}"))
    }
}

/// One line per dial, oldest first, each stamped with its milliseconds since `spawn`.
struct DialRecord<'a> {
    dials: &'a [Dial],
    spawned: Instant,
}

impl fmt::Display for DialRecord<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\ndial record ({}):", plural(self.dials.len(), "dial"))?;
        for dial in self.dials {
            let since = dial.at.saturating_duration_since(self.spawned).as_millis();
            write!(f, "\n  {since} ms: {dial}")?;
        }
        Ok(())
    }
}

/// Prints the octets only inside the fixture range; any other address may name someone's device.
struct Device(BdAddr);

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.octets() {
            [o0, o1, o2, o3, o4, o5] if [o0, o1, o2] == TEST_RANGE => {
                write!(f, "{o0:02x}:{o1:02x}:{o2:02x}:{o3:02x}:{o4:02x}:{o5:02x}")
            }
            _ => f.write_str("an address outside the test range"),
        }
    }
}

/// The symbolic name of each errno the trap answers or a route is likely to give, else its number.
struct Name(Errno);

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self.0 {
            Errno::HOSTDOWN => "EHOSTDOWN",
            Errno::PROTONOSUPPORT => "EPROTONOSUPPORT",
            Errno::INVAL => "EINVAL",
            Errno::BADF => "EBADF",
            Errno::CONNREFUSED => "ECONNREFUSED",
            Errno::TIMEDOUT => "ETIMEDOUT",
            Errno::HOSTUNREACH => "EHOSTUNREACH",
            other => return write!(f, "errno {}", other.raw_os_error()),
        };
        f.write_str(name)
    }
}

impl fmt::Display for Dial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            device,
            psm,
            outcome,
            ..
        } = self;
        write!(f, "connect to {} PSM {psm:#06x}: ", Device(*device))?;
        match outcome {
            Outcome::Pending => f.write_str("not answered yet"),
            Outcome::Accepted => f.write_str("accepted"),
            Outcome::Refused(errno) => write!(f, "refused with {}", Name(*errno)),
            Outcome::Unrouted => write!(f, "no route, answered {}", Name(Errno::HOSTDOWN)),
            Outcome::Abandoned => f.write_str("the caller was gone before the answer"),
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::ForeignSocket { kind, protocol } => write!(
                f,
                "socket(AF_BLUETOOTH, type {kind}, protocol {protocol}): only SOCK_SEQPACKET with protocol 0 is served, answered {}",
                Name(Errno::PROTONOSUPPORT)
            ),
            Self::Unrouted { device, psm } => write!(
                f,
                "connect to {} PSM {psm:#06x}: no route, answered {}",
                Device(device),
                Name(Errno::HOSTDOWN)
            ),
            Self::BadAddress { cid, address_type } => write!(
                f,
                "connect to channel id {cid:#06x} with address type {address_type}: only a PSM on a classic address is served, answered {}",
                Name(Errno::INVAL)
            ),
            Self::Stranger { fd } => write!(
                f,
                "connect on descriptor {fd} with an L2CAP address: the trap did not hand out that socket, answered {}",
                Name(Errno::BADF)
            ),
        }
    }
}

impl fmt::Debug for Answer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Accept { after, .. } => f
                .debug_struct("Accept")
                .field("after", after)
                .finish_non_exhaustive(),
            Self::Refuse { after, errno } => f
                .debug_struct("Refuse")
                .field("after", after)
                .field("errno", &format_args!("{}", Name(*errno)))
                .finish(),
        }
    }
}

impl fmt::Debug for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Route")
            .field("psm", &format_args!("{:#06x}", self.psm))
            .field("answer", &self.answer)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(last: u8) -> BdAddr {
        BdAddr::from([0x02, 0x00, 0x00, 0x00, 0x00, last])
    }

    #[test]
    fn a_violation_names_the_call_the_address_and_the_psm() {
        let unrouted = Violation::Unrouted {
            device: fixture(0x09),
            psm: 0x1001,
        };
        assert_eq!(
            unrouted.to_string(),
            "connect to 02:00:00:00:00:09 PSM 0x1001: no route, answered EHOSTDOWN"
        );
        let foreign = Violation::ForeignSocket {
            kind: 3,
            protocol: 1,
        };
        assert_eq!(
            foreign.to_string(),
            "socket(AF_BLUETOOTH, type 3, protocol 1): only SOCK_SEQPACKET with protocol 0 is served, answered EPROTONOSUPPORT"
        );
        let bad = Violation::BadAddress {
            cid: 4,
            address_type: 1,
        };
        assert!(
            bad.to_string().starts_with("connect to channel id 0x0004"),
            "{bad}"
        );
        assert!(bad.to_string().ends_with("answered EINVAL"), "{bad}");
        let stranger = Violation::Stranger { fd: 7 };
        assert!(
            stranger.to_string().starts_with("connect on descriptor 7"),
            "{stranger}"
        );
        assert!(
            stranger.to_string().ends_with("answered EBADF"),
            "{stranger}"
        );
    }

    #[test]
    fn an_address_outside_the_test_range_is_never_printed() {
        let device = BdAddr::from([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let spawned = Instant::now();
        let dial = Dial {
            at: spawned,
            device,
            psm: 0x1001,
            outcome: Outcome::Refused(Errno::CONNREFUSED),
        };
        let error = TrapError::Violations {
            violations: vec![Violation::Unrouted {
                device,
                psm: 0x1001,
            }],
            dials: vec![dial.clone()],
            spawned,
        };
        for text in [dial.to_string(), error.to_string(), format!("{error:?}")] {
            assert!(!text.contains("11:22:33:44:55:66"), "{text}");
        }
        assert_eq!(
            dial.to_string(),
            "connect to an address outside the test range PSM 0x1001: refused with ECONNREFUSED"
        );
    }

    #[test]
    fn the_record_prints_every_violation_then_every_dial() {
        let spawned = Instant::now();
        let dial = |outcome| Dial {
            at: spawned + Duration::from_millis(40),
            device: fixture(0x01),
            psm: 0x001f,
            outcome,
        };
        let error = TrapError::Violations {
            violations: vec![Violation::Stranger { fd: 7 }],
            dials: vec![dial(Outcome::Accepted), dial(Outcome::Abandoned)],
            spawned,
        };
        let text = error.to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5, "{text}");
        assert_eq!(lines[0], "the child made 1 call the trap refused:");
        assert!(lines[1].starts_with("  connect on descriptor 7"), "{text}");
        assert_eq!(lines[2], "dial record (2 dials):");
        assert_eq!(
            lines[3],
            "  40 ms: connect to 02:00:00:00:00:01 PSM 0x001f: accepted"
        );
        assert_eq!(
            lines[4],
            "  40 ms: connect to 02:00:00:00:00:01 PSM 0x001f: the caller was gone before the answer"
        );
    }
}
