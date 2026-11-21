# Hardware tests

`cargo xtask hw link` runs the daemon's link checks against real AirPods. The
Rust test suite covers everything that can be driven without a radio; these
checks cover what cannot: adopting a link BlueZ already holds, reconnecting,
idle wakeups, and whether the AirPods confirm each command.

A person starts every run by hand, on a commit of this repository itself. No
runner is registered for it, so code from a fork never reaches the machine that
holds the AirPods.

## What it needs

- The machine with the AirPods paired and connected, in a graphical session
  with BlueZ, PipeWire and the installed `librepods.service`.
- A release build of `librepods` and of `xtask` copied to a private temporary
  directory on that machine:

  ```bash
  cargo build -p librepods -p xtask --release --locked
  ```

- The installed `librepods-ctl` under `~/.local/bin`, which check C3 compares
  against.

Then, on that machine:

```bash
./xtask hw link --binary ./librepods                 # unattended checks
./xtask hw link --binary ./librepods --attended      # also the command census
./xtask hw link --binary ./librepods --attended --adapter-cycle
```

`--attended` means a person is at the machine: the census ends by asking for
both buds to be taken out and Enter pressed on standard input, within 5
minutes. `--adapter-cycle` needs `--attended`, because it powers the adapter
off and on and every Bluetooth device on the machine drops once.

## What it stops and what it changes

1. It takes an exclusive lock on `librepods-hw.lock` in
   `$XDG_RUNTIME_DIR` and holds it until after the restore; a second run
   started meanwhile ends at once, having stopped nothing. It also refuses
   to start if a user unit named `librepods-hw` exists, which means an
   earlier run left a failed unit
   (`systemctl --user reset-failed librepods-hw` clears that).
2. It notes whether `librepods.service` is running and stops it. From that
   moment the installed service is restored on every way out: success, a
   failed check, a panic, SIGINT, SIGTERM, SIGHUP, or a tool the run needs
   that fails or hangs and is killed at its timeout.
3. It starts the binary under test as the transient user unit `librepods-hw`,
   with every hardening directive of `daemon/librepods.service` from `UMask`
   on, except the ones naming the installed daemon's own directories. Its
   runtime and state directories are a private temporary directory, so the
   run never touches the real socket or state file. It uses the real system
   bus and the real session bus.
4. After the checks it stops `librepods-hw` and starts `librepods.service`
   again if it was running before.

Every program it runs is waited for before it exits, or killed at its
timeout. SIGKILL cannot be caught, so after one nothing is restored: run
`systemctl --user stop librepods-hw` and `systemctl --user start
librepods.service` by hand.

It stops no other unit. It never forgets, pairs or trusts a device, never runs
a card profile command, and puts back every setting the census changes,
reporting any it could not. The adapter is powered back on whatever ends the adapter check.

Expect the panel to show nothing for the whole run (about 30 minutes, longer
with `--attended`), and the AirPods audio to stop for a few seconds at each of
the 5 reconnects of C4, for the whole window of C5 with nothing connected, and
once more in the adapter check with `--adapter-cycle`.

## The checks

| Check | What it does | Passes when |
|---|---|---|
| C1 adoption | restarts the unit 10 times with the AirPods connected | each restart reaches a populated state |
| C2 stop | stops the unit with the link up | the unit is inactive within 1 s, the socket and state file are gone, and BlueZ still holds the device |
| C3 same contract | reads `librepods status` and the installed `librepods-ctl status` | the keys are exactly the status document's, and both print the same line |
| C4 reconnect | `do disconnect` then `do connect`, 5 times | `connected` is false within 5 s and the state is populated again within 15 s |
| C5 idle | 10 minutes with the link up, then 10 minutes after `do disconnect` | the daemon's wakeups stay within the bounds below and the adapter is never discovering |
| C6 commands | each command of the census 10 times, then one with no bud in (`--attended`) | every command answers its expected word 10 of 10 (`confirmed`, or `sent` for the adaptive level, which alternates between 40 and 60 so each trial is a write), and the last answers `not_worn` |
| C7 adapter | `bluetoothctl power off`, `power on`, `do connect` (`--adapter-cycle`) | `connected` is true before and false after power off, and the state is populated again |

C6 sends each command over protocol 1 on the test socket rather than through
`librepods set`, which prints nothing when a command succeeds and so cannot
show the reply. One 10 s bound covers each command's connect, request and
reply. The first command that gets no reply at all within it ends the census
as failed and is named, and putting the settings back stops at its first
unanswered command too; a `timeout` reply is an answer.
A setting already at the value its first row asks for is first set to its
other value, once and unmeasured, so every trial is a write.
C6 needs AirPods whose status shows noise control, adaptive mode,
conversational awareness and one bud noise control; with any of them missing
it fails, naming what is missing, and sets nothing.

Before the checks it prints whether a Bluetooth audio card sits in a headset
profile, because a connect can time out in that state.

Timings are printed as a range with the number of trials, never as one number:

| Gate | What | Bound |
|---|---|---|
| G1a | unit start to populated, from the journal's own stamps | at most 2 s in 10 of 10 |
| G1b | `link: idle to opening` to populated, after `do connect` | at most 500 ms in 4 of 5; a miss prints its phase lines |
| G2b | wall time of each census command | every confirmable trial answers before the daemon's confirm timeout |
| G3 | voluntary context switches of the daemon, a minute | at most 12 with the link up, at most 2 with nothing connected, printed beside the number of `librepods watch` lines in the same window |
| G4b | `do connect` reply to populated | at most 15 s in 5 of 5 |
| G4c | adapter power on to populated | reported only, no bound yet |

The output carries key names, booleans, counts and times. It never prints an
address, a device name or a serial number, and never echoes what a tool wrote
on its standard error.

## Exit status

| Status | Meaning |
|---|---|
| 0 | every check passed and every bound was met |
| 1 | a check failed or a bound was missed, the run was stopped by a signal, or a tool the run needs failed; `pactl` failing only prints the headset profile line as `unknown` |
| 2 | wrong usage, another run holds the lock, the lock in `$XDG_RUNTIME_DIR` could not be opened or taken, or a `librepods-hw` unit already exists; nothing was stopped |
| 77 | the AirPods did not connect within 20 s, so nothing was measured; the service was restored |
| 70 | the installed service could not be restored; this is printed last |
| 101 | the run panicked; the restore ran as it unwound, and its line follows the panic message |
