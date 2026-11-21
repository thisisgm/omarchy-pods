# pods-trap

## What it is

A test tool. It starts a program, usually the unmodified daemon binary, and
answers that program's Bluetooth `socket` and `connect` calls from the test
process, so the program's link code runs end to end on a machine with no
Bluetooth radio. It is a dev-dependency only and is never linked into the
daemon.

## How it works

1. **Filter.** `spawn` gives the command a `pre_exec` hook. Between fork and
   exec the child sets `no_new_privs` and installs a seccomp filter that hands
   two calls to the test: `socket` with family `AF_BLUETOOTH`, and `connect`
   with an address 14 bytes long (the size of `struct sockaddr_l2`). The child
   sends the filter's listener back over a socket pair, and a thread named
   `trap` in the test answers every call that arrives on it.
2. **Socket.** A `socket(AF_BLUETOOTH, SOCK_SEQPACKET, 0)` call gets one end of
   an `AF_UNIX` `SOCK_SEQPACKET` pair, put straight into the child. The trap
   keeps the other end. Any other Bluetooth socket is refused.
3. **Connect.** The trap reads the address from the child's memory. An address
   of another family continues to the kernel untouched. An L2CAP address on a
   socket the trap handed out is looked up in the routes: `Accept` returns 0
   and passes the far end to the route's `serve`, `Refuse` fails the call with
   the route's errno, and both wait their `after` first. No route fails the
   call with `EHOSTDOWN`.
4. **Record.** Every dial is recorded with its time, device, PSM and outcome.
   `dials` and `wait_dials` read the record while the child runs, and `finish`
   stops the thread and returns it.

## What a failure prints

A call the trap fails on its own account, rather than through a route's
`Refuse`, is a violation, and `finish` then returns
`TrapError::Violations`, which prints each violation and then the whole dial
record:

```text
the child made 4 calls the trap refused:
  socket(AF_BLUETOOTH, type 3, protocol 1): only SOCK_SEQPACKET with protocol 0 is served, answered EPROTONOSUPPORT
  connect to 02:00:00:00:00:09 PSM 0x1001: no route, answered EHOSTDOWN
  connect to channel id 0x0004 with address type 1: only a PSM on a classic address is served, answered EINVAL
  connect on descriptor 7 with an L2CAP address: the trap did not hand out that socket, answered EBADF
dial record (2 dials):
  12 ms: connect to 02:00:00:00:00:01 PSM 0x1001: accepted
  15 ms: connect to 02:00:00:00:00:09 PSM 0x1001: no route, answered EHOSTDOWN
```

A `wait_dials` that runs out of time prints the same record after a line such
as `waited 200 ms for 1 dial and none came`. Only addresses that start with
`02:00:00`, the range every fixture uses, are printed; any other prints as
`an address outside the test range`.

## Debugging a failing run

```sh
strace -f -e trace=seccomp,socket,connect <test binary> <test name> --exact --nocapture
```

shows each call of the child with the value the trap returned. `ENOSYS` from
`socket` means the trap was already gone: its thread had stopped, so the
kernel found no listener for the call.

## Limits

- x86-64 Linux only; the filter holds that architecture's system call numbers.
- The kernel must offer seccomp user notification
  (`SECCOMP_RET_USER_NOTIF`), its killable wait
  (`SECCOMP_FILTER_FLAG_WAIT_KILLABLE_RECV`) and the atomic descriptor answer
  (`SECCOMP_ADDFD_FLAG_SEND`). A kernel without the first gives
  `TrapError::Unsupported`; one without either of the others fails the
  install in the child, which reaches the test as `TrapError::Spawn` with
  `EINVAL`. Run the tests once to learn whether a kernel qualifies.
- One answer at a time: while one answer waits its delay, the child's next
  trapped call waits behind it.
- `serve` runs on the trap's thread, so it hands the channel on and returns.
- The pair is not a radio. It does not show a real connect timeout, the errno
  a real device refuses with, or the channel's MTU.
