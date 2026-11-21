//! What the trap does with each call it is handed, decided from the call's values alone.

use pods_proto::addr::BdAddr;

use crate::record::Answer;

const SOCK_SEQPACKET: u32 = libc::SOCK_SEQPACKET.cast_unsigned();
const SOCK_NONBLOCK: u32 = libc::SOCK_NONBLOCK.cast_unsigned();
/// The flags a caller may add to a socket's type; neither changes whether the call is served.
const TYPE_FLAGS: u32 = libc::SOCK_CLOEXEC.cast_unsigned() | SOCK_NONBLOCK;
/// `BTPROTO_L2CAP` in include/net/bluetooth/bluetooth.h.
const BTPROTO_L2CAP: u32 = 0;
const AF_BLUETOOTH: u16 = 31;
/// `l2_cid` is zero for a channel that is reached through a PSM.
const NO_FIXED_CHANNEL: u16 = 0;
/// `l2_bdaddr_type` for a classic link, `BDADDR_BREDR` in include/net/bluetooth/bluetooth.h.
const BDADDR_BREDR: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketVerdict {
    Serve,
    Foreign { kind: u32, protocol: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Address {
    Other,
    L2cap {
        device: BdAddr,
        psm: u16,
        cid: u16,
        address_type: u8,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum ConnectVerdict {
    Continue,
    Stranger,
    BadAddress {
        cid: u16,
        address_type: u8,
    },
    Unrouted {
        device: BdAddr,
        psm: u16,
    },
    Routed {
        device: BdAddr,
        psm: u16,
        answer: Answer,
    },
}

/// A system call passes an `int` in a 64 bit register whose high half the kernel ignores.
pub(crate) fn low_half(argument: u64) -> u32 {
    let [b0, b1, b2, b3, ..] = argument.to_le_bytes();
    u32::from_le_bytes([b0, b1, b2, b3])
}

/// `args` are those of a `socket` call whose family the filter already found to be `AF_BLUETOOTH`.
pub(crate) fn socket(args: &[u64; 6]) -> SocketVerdict {
    let [_, kind, protocol, ..] = *args;
    let kind = low_half(kind) & !TYPE_FLAGS;
    let protocol = low_half(protocol);
    if kind == SOCK_SEQPACKET && protocol == BTPROTO_L2CAP {
        SocketVerdict::Serve
    } else {
        SocketVerdict::Foreign { kind, protocol }
    }
}

/// True when a served `socket` call asked for `SOCK_NONBLOCK`, which its end must then carry.
pub(crate) fn nonblocking(args: &[u64; 6]) -> bool {
    let [_, kind, ..] = *args;
    low_half(kind) & SOCK_NONBLOCK != 0
}

// Sample input: `1f 00 01 10 66 55 44 33 22 11 00 00 00 00` is PSM 0x1001 on 11:22:33:44:55:66, channel id 0, a classic address.
#[expect(
    clippy::host_endian_bytes,
    reason = "the kernel reads sa_family_t in host order; the PSM and channel id are little endian on every host"
)]
pub(crate) fn address(bytes: &[u8; 14]) -> Address {
    let [
        f0,
        f1,
        p0,
        p1,
        a5,
        a4,
        a3,
        a2,
        a1,
        a0,
        c0,
        c1,
        address_type,
        _padding,
    ] = *bytes;
    if u16::from_ne_bytes([f0, f1]) != AF_BLUETOOTH {
        return Address::Other;
    }
    Address::L2cap {
        device: BdAddr::from([a0, a1, a2, a3, a4, a5]),
        psm: u16::from_le_bytes([p0, p1]),
        cid: u16::from_le_bytes([c0, c1]),
        address_type,
    }
}

/// `owned` says whether the trap handed out the descriptor; `route` is the answer set for the address's device and PSM.
pub(crate) fn connect(address: &Address, owned: bool, route: Option<&Answer>) -> ConnectVerdict {
    let Address::L2cap {
        device,
        psm,
        cid,
        address_type,
    } = *address
    else {
        return ConnectVerdict::Continue;
    };
    if !owned {
        return ConnectVerdict::Stranger;
    }
    if cid != NO_FIXED_CHANNEL || address_type != BDADDR_BREDR {
        return ConnectVerdict::BadAddress { cid, address_type };
    }
    match route {
        None => ConnectVerdict::Unrouted { device, psm },
        Some(answer) => ConnectVerdict::Routed {
            device,
            psm,
            answer: answer.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use proptest::prelude::*;
    use rustix::io::Errno;

    use super::*;

    const SAMPLE: [u8; 14] = [
        0x1f, 0x00, 0x01, 0x10, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00, 0x00, 0x00, 0x00,
    ];

    fn device() -> BdAddr {
        "11:22:33:44:55:66".parse().unwrap()
    }

    fn socket_args(kind: i32, protocol: u64) -> [u64; 6] {
        [31, u64::from(kind.cast_unsigned()), protocol, 0, 0, 0]
    }

    fn refuse() -> Answer {
        Answer::Refuse {
            after: Duration::ZERO,
            errno: Errno::CONNREFUSED,
        }
    }

    #[test]
    fn an_l2cap_seqpacket_socket_is_served() {
        for kind in [
            libc::SOCK_SEQPACKET,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
        ] {
            assert_eq!(
                socket(&socket_args(kind, 0)),
                SocketVerdict::Serve,
                "type {kind:#x}"
            );
        }
        // The kernel reads an `int`, so a high half left in the register changes nothing.
        let high_half = socket_args(libc::SOCK_SEQPACKET, 0xffff_ffff_0000_0000);
        assert_eq!(socket(&high_half), SocketVerdict::Serve);
    }

    #[test]
    fn only_sock_nonblock_asks_for_a_nonblocking_end() {
        for (kind, expected) in [
            (libc::SOCK_SEQPACKET, false),
            (libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, false),
            (libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK, true),
            (
                libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                true,
            ),
        ] {
            assert_eq!(
                nonblocking(&socket_args(kind, 0)),
                expected,
                "type {kind:#x}"
            );
        }
    }

    #[test]
    fn any_other_type_or_protocol_is_foreign() {
        for (kind, protocol, expected) in [
            (libc::SOCK_RAW, 1, (3, 1)),
            (libc::SOCK_STREAM, 3, (1, 3)),
            (libc::SOCK_SEQPACKET, 1, (5, 1)),
            (libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0, (2, 0)),
        ] {
            let (kind_out, protocol_out) = expected;
            assert_eq!(
                socket(&socket_args(kind, protocol)),
                SocketVerdict::Foreign {
                    kind: kind_out,
                    protocol: protocol_out
                }
            );
        }
    }

    #[test]
    fn the_sample_is_psm_0x1001_on_its_device() {
        assert_eq!(
            address(&SAMPLE),
            Address::L2cap {
                device: device(),
                psm: 0x1001,
                cid: 0,
                address_type: 0
            }
        );
    }

    #[test]
    fn an_address_of_another_family_continues() {
        let mut unix = SAMPLE;
        unix[0] = 0x01;
        assert_eq!(address(&unix), Address::Other);
        assert!(matches!(
            connect(&address(&unix), true, None),
            ConnectVerdict::Continue
        ));
        assert!(matches!(
            connect(&address(&unix), false, None),
            ConnectVerdict::Continue
        ));
    }

    #[test]
    fn a_fixed_channel_or_an_le_address_is_a_bad_address() {
        let mut fixed = SAMPLE;
        fixed[10] = 0x04;
        let mut le = SAMPLE;
        le[12] = 0x01;
        for (bytes, cid, address_type) in [(fixed, 4, 0), (le, 0, 1)] {
            let verdict = connect(&address(&bytes), true, Some(&refuse()));
            assert!(
                matches!(verdict, ConnectVerdict::BadAddress { cid: c, address_type: t } if c == cid && t == address_type),
                "{verdict:?}"
            );
        }
    }

    #[test]
    fn an_l2cap_address_on_a_strange_descriptor_is_a_stranger() {
        let verdict = connect(&address(&SAMPLE), false, Some(&refuse()));
        assert!(matches!(verdict, ConnectVerdict::Stranger), "{verdict:?}");
    }

    #[test]
    fn the_route_decides_accept_refuse_or_host_down() {
        let accept = Answer::Accept {
            after: Duration::ZERO,
            serve: Arc::new(drop),
        };
        let verdict = connect(&address(&SAMPLE), true, Some(&accept));
        assert!(
            matches!(verdict, ConnectVerdict::Routed { psm: 0x1001, answer: Answer::Accept { .. }, device: found } if found == device()),
            "{verdict:?}"
        );
        let verdict = connect(&address(&SAMPLE), true, Some(&refuse()));
        assert!(
            matches!(
                verdict,
                ConnectVerdict::Routed {
                    answer: Answer::Refuse {
                        errno: Errno::CONNREFUSED,
                        ..
                    },
                    ..
                }
            ),
            "{verdict:?}"
        );
        let verdict = connect(&address(&SAMPLE), true, None);
        assert!(
            matches!(verdict, ConnectVerdict::Unrouted { psm: 0x1001, device: found } if found == device()),
            "{verdict:?}"
        );
    }

    proptest! {
        #[test]
        fn any_fourteen_bytes_get_a_verdict(bytes in any::<[u8; 14]>()) {
            let parsed = address(&bytes);
            // `1f 00` is `AF_BLUETOOTH` in host order on x86-64, the only host the crate builds for.
            if bytes[..2] != [0x1f, 0x00] {
                prop_assert_eq!(parsed, Address::Other);
            }
            let _verdict = connect(&parsed, true, None);
        }
    }
}
