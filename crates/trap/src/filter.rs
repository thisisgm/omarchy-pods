//! The classic BPF program that hands a child's Bluetooth socket, connect and L2CAP option calls to the trap.

use libc::{SECCOMP_RET_ALLOW, SECCOMP_RET_USER_NOTIF, sock_filter};

#[cfg(not(target_arch = "x86_64"))]
compile_error!("pods-trap knows the system call numbers of x86-64 Linux only");

/// `BPF_LD | BPF_W | BPF_ABS` in `linux/bpf_common.h`.
const LOAD_WORD: u16 = 0x20;
/// `BPF_JMP | BPF_JEQ | BPF_K` in `linux/bpf_common.h`.
const JUMP_IF_EQUAL: u16 = 0x15;
/// `BPF_RET | BPF_K` in `linux/bpf_common.h`.
const RETURN: u16 = 0x06;

// Offsets into `struct seccomp_data` in `linux/seccomp.h`; an argument's low half comes first on x86-64.
const NUMBER: u32 = 0;
const ARCHITECTURE: u32 = 4;
const ARGUMENT_0: u32 = 16;
const ARGUMENT_1: u32 = 24;
const ARGUMENT_2: u32 = 32;

/// `#define AUDIT_ARCH_X86_64 (EM_X86_64|__AUDIT_ARCH_64BIT|__AUDIT_ARCH_LE)` in `linux/audit.h`.
const AUDIT_ARCH_X86_64: u32 = 0x3e | 0x8000_0000 | 0x4000_0000;
/// `#define __NR_socket 41` in `asm/unistd_64.h`.
const NR_SOCKET: u32 = 41;
/// `#define __NR_connect 42` in `asm/unistd_64.h`.
const NR_CONNECT: u32 = 42;
/// `#define __NR_setsockopt 54` in `asm/unistd_64.h`.
const NR_SETSOCKOPT: u32 = 54;
/// `#define __NR_getsockopt 55` in `asm/unistd_64.h`.
const NR_GETSOCKOPT: u32 = 55;
/// The legacy L2CAP socket option level in include/net/bluetooth/bluetooth.h.
pub(crate) const SOL_L2CAP: u32 = 6;
/// The size of `struct sockaddr_l2` in `include/net/bluetooth/l2cap.h`.
const SOCKADDR_L2_LEN: u32 = 14;
const AF_BLUETOOTH: u32 = libc::AF_BLUETOOTH.cast_unsigned();

const fn statement(code: u16, k: u32) -> sock_filter {
    sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

/// `jt` and `jf` count the instructions skipped after this one.
const fn jump_if_equal(k: u32, jt: u8, jf: u8) -> sock_filter {
    sock_filter {
        code: JUMP_IF_EQUAL,
        jt,
        jf,
        k,
    }
}

pub(crate) const PROGRAM: [sock_filter; 16] = [
    statement(LOAD_WORD, ARCHITECTURE),
    jump_if_equal(AUDIT_ARCH_X86_64, 1, 0),
    statement(RETURN, SECCOMP_RET_ALLOW),
    statement(LOAD_WORD, NUMBER),
    jump_if_equal(NR_CONNECT, 0, 2),
    statement(LOAD_WORD, ARGUMENT_2),
    jump_if_equal(SOCKADDR_L2_LEN, 8, 7),
    jump_if_equal(NR_SOCKET, 0, 2),
    statement(LOAD_WORD, ARGUMENT_0),
    jump_if_equal(AF_BLUETOOTH, 5, 4),
    jump_if_equal(NR_GETSOCKOPT, 1, 0),
    jump_if_equal(NR_SETSOCKOPT, 0, 2),
    statement(LOAD_WORD, ARGUMENT_1),
    jump_if_equal(SOL_L2CAP, 1, 0),
    statement(RETURN, SECCOMP_RET_ALLOW),
    statement(RETURN, SECCOMP_RET_USER_NOTIF),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_jump_lands_inside_the_program() {
        for (index, instruction) in PROGRAM.iter().enumerate() {
            if instruction.code == JUMP_IF_EQUAL {
                for skip in [instruction.jt, instruction.jf] {
                    let target = index + 1 + usize::from(skip);
                    assert!(
                        target < PROGRAM.len(),
                        "instruction {index} jumps to {target}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_program_ends_in_a_return() {
        assert_eq!(PROGRAM.last().map(|last| last.code), Some(RETURN));
    }

    #[test]
    fn the_local_numbers_are_the_kernel_numbers() {
        assert_eq!(
            u32::from(LOAD_WORD),
            libc::BPF_LD | libc::BPF_W | libc::BPF_ABS
        );
        assert_eq!(
            u32::from(JUMP_IF_EQUAL),
            libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K
        );
        assert_eq!(u32::from(RETURN), libc::BPF_RET | libc::BPF_K);
        assert_eq!(i64::from(NR_SOCKET), libc::SYS_socket);
        assert_eq!(i64::from(NR_CONNECT), libc::SYS_connect);
        assert_eq!(i64::from(NR_SETSOCKOPT), libc::SYS_setsockopt);
        assert_eq!(i64::from(NR_GETSOCKOPT), libc::SYS_getsockopt);
    }
}
